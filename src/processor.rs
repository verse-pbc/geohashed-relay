use anyhow::Result;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{EventContext, EventProcessor, StoreCommand, Error as RelayError};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info};
use crate::geohash_utils::extract_geohash_tags;

/// Per-connection state for tracking
#[derive(Debug, Clone, Default)]
pub struct ConnectionState {
    pub events_sent: u64,
    pub first_event_time: Option<Instant>,
    pub subdomain_info: Option<String>,
}


/// Multi-tenant event processor with geohash-based location routing
#[derive(Debug, Clone)]
pub struct GeohashedEventProcessor {
}

impl GeohashedEventProcessor {
    pub fn new() -> Self {
        Self {
        }
    }
}

impl EventProcessor<ConnectionState> for GeohashedEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<RwLock<ConnectionState>>,
        context: &EventContext,
    ) -> Result<Vec<StoreCommand>, RelayError> {
        // Initialize connection state if needed
        let mut state = custom_state.write();
        let now = Instant::now();
        
        if state.first_event_time.is_none() {
            state.first_event_time = Some(now);
            state.subdomain_info = match context.subdomain.as_ref() {
                nostr_lmdb::Scope::Named { name, .. } => Some(name.clone()),
                nostr_lmdb::Scope::Default => None,
            };
        }
        
        // Track events sent
        state.events_sent += 1;
        
        // Check for geohash tags and determine target scope
        let tags: Vec<Vec<String>> = event.tags.iter()
            .map(|tag| tag.clone().to_vec())
            .collect();
        let geohash_tags = extract_geohash_tags(&tags);
        
        // Extract the current subdomain name
        let current_subdomain = match context.subdomain.as_ref() {
            nostr_lmdb::Scope::Named { name, .. } => Some(name.as_str()),
            nostr_lmdb::Scope::Default => None,
        };
        
        // If we're on a subdomain that's not a valid geohash, reject all events
        if let Some(subdomain) = current_subdomain {
            if !crate::geohash_utils::is_valid_geohash(subdomain) {
                return Err(RelayError::restricted(format!(
                    "restricted: '{}' is not a valid geohash subdomain",
                    subdomain
                )));
            }
        }
        
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
                    (*context.subdomain).clone(),
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
                (*context.subdomain).clone(),
                None,
            )])
        }
    }
    
    fn can_see_event(
        &self,
        _event: &Event,
        _custom_state: Arc<RwLock<ConnectionState>>,
        _context: &EventContext,
    ) -> Result<bool, RelayError> {
        // Event is visible to all
        Ok(true)
    }
    
    fn verify_filters(
        &self,
        filters: &[Filter],
        _custom_state: Arc<RwLock<ConnectionState>>,
        _context: &EventContext,
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