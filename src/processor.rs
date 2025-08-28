use anyhow::Result;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{EventContext, EventProcessor, StoreCommand, Error as RelayError};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use crate::geohash_utils::{extract_geohash_tags, is_geohash_subdomain};

/// Per-connection state for rate limiting and tracking
#[derive(Debug, Clone, Default)]
pub struct ConnectionState {
    pub events_sent: u64,
    pub first_event_time: Option<Instant>,
    pub rate_limit_info: RateLimitInfo,
    pub subdomain_info: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub events_received: u32,
    pub window_start: Instant,
}

impl Default for RateLimitInfo {
    fn default() -> Self {
        Self {
            events_received: 0,
            window_start: Instant::now(),
        }
    }
}

/// Multi-tenant event processor with geohash-based location routing
#[derive(Debug, Clone)]
pub struct GeohashedEventProcessor {
    allowed_subdomains: HashSet<String>,
    events_per_minute: u32,
    require_auth_for_write: bool,
    require_auth_for_read: bool,
}

impl GeohashedEventProcessor {
    pub fn new(
        allowed_subdomains: HashSet<String>,
        events_per_minute: u32,
        require_auth_for_write: bool,
        require_auth_for_read: bool,
    ) -> Self {
        Self {
            allowed_subdomains,
            events_per_minute,
            require_auth_for_write,
            require_auth_for_read,
        }
    }
    
    fn get_rate_limit(&self, _subdomain: &nostr_lmdb::Scope) -> u32 {
        // Same rate limit for all scopes
        self.events_per_minute
    }
    
    fn is_subdomain_allowed(&self, subdomain: &nostr_lmdb::Scope) -> bool {
        match subdomain {
            nostr_lmdb::Scope::Named { name, .. } => {
                // Always allow valid geohash subdomains
                if is_geohash_subdomain(name) {
                    return true;
                }
                
                // If we have a whitelist, check it
                if !self.allowed_subdomains.is_empty() {
                    self.allowed_subdomains.contains(name)
                } else {
                    // If no whitelist, all subdomains are allowed
                    true
                }
            }
            nostr_lmdb::Scope::Default => true, // Root domain always allowed
        }
    }
}

impl EventProcessor<ConnectionState> for GeohashedEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<RwLock<ConnectionState>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, RelayError> {
        // Check subdomain access
        if !self.is_subdomain_allowed(&context.subdomain) {
            warn!(
                "Rejected event from disallowed subdomain: {:?}",
                context.subdomain
            );
            return Err(RelayError::restricted("subdomain not allowed"));
        }
        
        // Check authentication requirements
        if self.require_auth_for_write && context.authed_pubkey.is_none() {
            warn!("Rejected event from unauthenticated user");
            return Err(RelayError::auth_required("authentication required for writing"));
        }
        
        // Rate limiting
        let mut state = custom_state.write();
        let now = Instant::now();
        
        // Initialize connection state if needed
        if state.first_event_time.is_none() {
            state.first_event_time = Some(now);
            state.subdomain_info = match &context.subdomain {
                nostr_lmdb::Scope::Named { name, .. } => Some(name.clone()),
                nostr_lmdb::Scope::Default => None,
            };
            state.rate_limit_info.window_start = now;
        }
        
        // Reset rate limit window if needed
        if now.duration_since(state.rate_limit_info.window_start) > Duration::from_secs(60) {
            state.rate_limit_info.events_received = 0;
            state.rate_limit_info.window_start = now;
        }
        
        // Check rate limit
        state.rate_limit_info.events_received += 1;
        let limit = self.get_rate_limit(&context.subdomain);
        
        if state.rate_limit_info.events_received > limit {
            warn!(
                "Rate limit exceeded for pubkey {}: {} events in window (limit: {})",
                event.pubkey,
                state.rate_limit_info.events_received,
                limit
            );
            return Err(RelayError::restricted(
                format!("rate limit exceeded: max {} events per minute", limit)
            ));
        }
        
        // Track events sent
        state.events_sent += 1;
        
        // Check for geohash tags and determine target scope
        let tags: Vec<Vec<String>> = event.tags.iter()
            .map(|tag| tag.clone().to_vec())
            .collect();
        let geohash_tags = extract_geohash_tags(&tags);
        
        let storage_scope = if let Some(first_geohash) = geohash_tags.first() {
            // Auto-forward to geohash scope
            info!(
                "Auto-forwarding event {} to geohash scope '{}' (posted via {:?})",
                event.id,
                first_geohash,
                context.subdomain
            );
            // Create a named scope for the geohash
            nostr_lmdb::Scope::named(first_geohash).unwrap_or(context.subdomain.clone())
        } else {
            // Use connection's subdomain scope
            context.subdomain.clone()
        };
        
        // Log event acceptance
        info!(
            "Accepted event {} from {} - storing in scope {:?} (auth: {:?})",
            event.id,
            event.pubkey,
            storage_scope,
            context.authed_pubkey
        );
        
        // Store the event with proper scope isolation
        // Note: OK response will be ["OK", event_id, true, ""] per NIP-01
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            storage_scope,
            None,
        )])
    }
    
    fn can_see_event(
        &self,
        _event: &Event,
        _custom_state: Arc<RwLock<ConnectionState>>,
        context: EventContext<'_>,
    ) -> Result<bool, RelayError> {
        // Check if subdomain is allowed
        if !self.is_subdomain_allowed(&context.subdomain) {
            return Ok(false);
        }
        
        // Check read authentication requirements
        if self.require_auth_for_read && context.authed_pubkey.is_none() {
            return Ok(false);
        }
        
        // Event is visible
        Ok(true)
    }
    
    fn verify_filters(
        &self,
        filters: &[Filter],
        _custom_state: Arc<RwLock<ConnectionState>>,
        context: EventContext<'_>,
    ) -> Result<(), RelayError> {
        // Check subdomain access
        if !self.is_subdomain_allowed(&context.subdomain) {
            return Err(RelayError::restricted("subdomain not allowed"));
        }
        
        // Check read authentication if required
        if self.require_auth_for_read && context.authed_pubkey.is_none() {
            return Err(RelayError::auth_required("authentication required for reading"));
        }
        
        // Basic filter validation
        for filter in filters {
            // You can add custom filter validation here
            // For example, limit time ranges, number of authors, etc.
            debug!("Verified filter: {:?}", filter);
        }
        
        Ok(())
    }
}

#[cfg(test)]
#[path = "processor_tests.rs"]
mod processor_tests;