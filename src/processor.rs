use anyhow::Result;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{EventContext, EventProcessor, StoreCommand, Error as RelayError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use crate::geohash_utils::extract_geohash_tags;

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
    events_per_minute: u32,
}

impl GeohashedEventProcessor {
    pub fn new(
        events_per_minute: u32,
    ) -> Self {
        Self {
            events_per_minute,
        }
    }
    
    fn get_rate_limit(&self, _subdomain: &nostr_lmdb::Scope) -> u32 {
        // Same rate limit for all scopes
        self.events_per_minute
    }
}

impl EventProcessor<ConnectionState> for GeohashedEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<RwLock<ConnectionState>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, RelayError> {
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
        
        // Extract the current subdomain name
        let current_subdomain = match &context.subdomain {
            nostr_lmdb::Scope::Named { name, .. } => Some(name.as_str()),
            nostr_lmdb::Scope::Default => None,
        };
        
        // Check if event has a geohash tag
        if let Some(first_geohash) = geohash_tags.first() {
            // Event has a geohash tag - check if we're on the correct subdomain
            let is_correct_scope = match current_subdomain {
                Some(subdomain) => subdomain == first_geohash,
                None => false, // Root domain never accepts geotagged events
            };
            
            if is_correct_scope {
                // We're on the correct subdomain - store the event
                info!(
                    "Storing event {} with matching geohash '{}'",
                    event.id,
                    first_geohash
                );
                Ok(vec![StoreCommand::SaveSignedEvent(
                    Box::new(event),
                    context.subdomain.clone(),
                    None,
                )])
            } else {
                // Wrong subdomain - reject with helpful error message
                let message = if current_subdomain.is_none() {
                    format!(
                        "restricted: root relay does not accept geotagged events; use wss://{}.hashstr.com",
                        first_geohash
                    )
                } else {
                    format!(
                        "restricted: events with geohash '{}' must be posted to wss://{}.hashstr.com",
                        first_geohash,
                        first_geohash
                    )
                };
                
                info!(
                    "Rejecting event {} with geohash '{}' (posted to {:?})",
                    event.id,
                    first_geohash,
                    context.subdomain
                );
                
                Err(RelayError::restricted(message))
            }
        } else {
            // No geohash tag - store in current scope
            info!(
                "Storing event {} without geohash in scope {:?}",
                event.id,
                context.subdomain
            );
            Ok(vec![StoreCommand::SaveSignedEvent(
                Box::new(event),
                context.subdomain.clone(),
                None,
            )])
        }
    }
    
    fn can_see_event(
        &self,
        _event: &Event,
        _custom_state: Arc<RwLock<ConnectionState>>,
        _context: EventContext<'_>,
    ) -> Result<bool, RelayError> {
        // Event is visible to all
        Ok(true)
    }
    
    fn verify_filters(
        &self,
        filters: &[Filter],
        _custom_state: Arc<RwLock<ConnectionState>>,
        _context: EventContext<'_>,
    ) -> Result<(), RelayError> {
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