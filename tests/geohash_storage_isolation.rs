/// Integration tests for geohash storage and query isolation
/// 
/// These tests verify that events stored in one scope are NOT visible 
/// when querying from a different scope, confirming proper isolation.
/// 
/// Since we can't directly access the Store, we test the isolation
/// through the EventProcessor interface and verify the correct
/// StoreCommand scopes are generated.

use nostr_sdk::prelude::*;
use nostr_lmdb::Scope;
use relay_builder::{EventContext, EventProcessor, StoreCommand};
use geohashed_relay::processor::{ConnectionState, GeohashedEventProcessor};
use std::sync::Arc;
use parking_lot::RwLock;

/// Helper to create an event with a geohash tag
async fn create_event_with_geohash(content: &str, geohash: &str) -> Event {
    let keys = Keys::generate();
    EventBuilder::text_note(content)
        .tags(vec![
            Tag::custom(TagKind::Custom("g".into()), vec![geohash.to_string()])
        ])
        .sign(&keys)
        .await
        .unwrap()
}

/// Helper to create an event without geohash
async fn create_regular_event(content: &str) -> Event {
    let keys = Keys::generate();
    EventBuilder::text_note(content)
        .sign(&keys)
        .await
        .unwrap()
}

/// Helper to create a test processor
fn create_test_processor() -> GeohashedEventProcessor {
    GeohashedEventProcessor::new(
        100,
    )
}

/// Helper to create an EventContext
fn create_context(scope: Scope) -> EventContext<'static> {
    let keys = Keys::generate();
    let relay_pubkey = Box::leak(Box::new(keys.public_key()));
    let subdomain_ref = Box::leak(Box::new(scope));
    EventContext {
        relay_pubkey,
        subdomain: subdomain_ref,
        authed_pubkey: None,
    }
}

#[tokio::test]
async fn test_geohash_auto_forwarding_creates_correct_scope() {
    let processor = create_test_processor();
    
    // Test 1: Event with geohash posted to root domain
    let event_with_geo = create_event_with_geohash("SF event", "drt2z").await;
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let root_context = create_context(Scope::Default);
    
    let result = processor.handle_event(event_with_geo.clone(), state.clone(), root_context).await;
    assert!(result.is_ok());
    
    let commands = result.unwrap();
    assert_eq!(commands.len(), 1);
    
    // Verify it's stored in geohash scope, not root
    match &commands[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "drt2z", "Event should be stored in geohash scope");
                }
                _ => panic!("Expected Named scope for geohash"),
            }
        }
        _ => panic!("Expected SaveSignedEvent"),
    }
    
    // Test 2: Same event posted from a different subdomain
    let team_context = create_context(Scope::named("team1").unwrap());
    let result2 = processor.handle_event(event_with_geo, Arc::new(RwLock::new(ConnectionState::default())), team_context).await;
    assert!(result2.is_ok());
    
    let commands2 = result2.unwrap();
    
    // Should still go to geohash scope, not team1
    match &commands2[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "drt2z", "Event should still be in geohash scope, not team1");
                }
                _ => panic!("Expected Named scope"),
            }
        }
        _ => panic!("Expected SaveSignedEvent"),
    }
}

#[tokio::test]
async fn test_events_without_geohash_stay_in_connection_scope() {
    let processor = create_test_processor();
    let event = create_regular_event("Regular event").await;
    
    // Test from different connection scopes
    let test_cases = vec![
        (Scope::Default, "root"),
        (Scope::named("team1").unwrap(), "team1"),
        (Scope::named("drt2z").unwrap(), "drt2z"),  // Even if connected via geohash subdomain
    ];
    
    for (connection_scope, expected_name) in test_cases {
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(connection_scope.clone());
        
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match (&connection_scope, scope) {
                    (Scope::Default, Scope::Default) => {
                        // Correct - stored at root
                    }
                    (Scope::Named { name: expected, .. }, Scope::Named { name: actual, .. }) => {
                        assert_eq!(actual, expected, 
                                   "Event without geohash should stay in connection scope");
                    }
                    _ => panic!("Scope mismatch for {}", expected_name),
                }
            }
            _ => panic!("Expected SaveSignedEvent"),
        }
    }
}

#[tokio::test]
async fn test_multiple_geohash_tags_use_first_only() {
    let processor = create_test_processor();
    
    // Create event with multiple geohash tags
    let keys = Keys::generate();
    let event = EventBuilder::text_note("Multi-location")
        .tags(vec![
            Tag::custom(TagKind::Custom("g".into()), vec!["drt2z".to_string()]),  // SF
            Tag::custom(TagKind::Custom("g".into()), vec!["9q8yy".to_string()]),  // LA
            Tag::custom(TagKind::Custom("g".into()), vec!["gbsuv".to_string()]),  // London
        ])
        .sign(&keys)
        .await
        .unwrap();
    
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let context = create_context(Scope::Default);
    
    let result = processor.handle_event(event, state, context).await;
    assert!(result.is_ok());
    
    let commands = result.unwrap();
    
    // Should use ONLY the first geohash
    match &commands[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "drt2z", "Should use only first geohash");
                    assert_ne!(name, "9q8yy", "Should not use second geohash");
                    assert_ne!(name, "gbsuv", "Should not use third geohash");
                }
                _ => panic!("Expected Named scope"),
            }
        }
        _ => panic!("Expected SaveSignedEvent"),
    }
}

#[tokio::test]
async fn test_invalid_geohash_falls_back_to_connection_scope() {
    let processor = create_test_processor();
    
    // Create event with invalid geohash
    let keys = Keys::generate();
    let event = EventBuilder::text_note("Bad geohash")
        .tags(vec![
            Tag::custom(TagKind::Custom("g".into()), vec!["invalid!".to_string()]),  // Invalid chars
            Tag::custom(TagKind::Custom("g".into()), vec!["toolonggeohash".to_string()]),  // Too long
        ])
        .sign(&keys)
        .await
        .unwrap();
    
    // Test from team1 subdomain
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let team_scope = Scope::named("team1").unwrap();
    let context = create_context(team_scope);
    
    let result = processor.handle_event(event, state, context).await;
    assert!(result.is_ok());
    
    let commands = result.unwrap();
    
    // Should fall back to connection scope (team1) since geohash is invalid
    match &commands[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "team1", 
                               "Invalid geohash should fall back to connection scope");
                }
                _ => panic!("Expected Named scope"),
            }
        }
        _ => panic!("Expected SaveSignedEvent"),
    }
}

#[tokio::test]
async fn test_geohash_scopes_are_isolated() {
    let processor = create_test_processor();
    
    // Create events for different geohash locations
    let sf_event = create_event_with_geohash("San Francisco event", "drt2z").await;
    let la_event = create_event_with_geohash("Los Angeles event", "9q8yy").await;
    let london_event = create_event_with_geohash("London event", "gbsuv").await;
    
    let events = vec![
        (sf_event, "drt2z"),
        (la_event, "9q8yy"),
        (london_event, "gbsuv"),
    ];
    
    // Process each event and verify correct scope assignment
    for (event, expected_geohash) in events {
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(Scope::Default);
        
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        match &commands[0] {
            StoreCommand::SaveSignedEvent(stored_event, scope, _) => {
                assert_eq!(stored_event.id, event.id);
                match scope {
                    Scope::Named { name, .. } => {
                        assert_eq!(name, expected_geohash, 
                                   "Event should be stored in its geohash scope");
                    }
                    _ => panic!("Expected Named scope"),
                }
            }
            _ => panic!("Expected SaveSignedEvent"),
        }
    }
}

#[tokio::test]
async fn test_same_event_different_endpoints_same_storage() {
    let processor = create_test_processor();
    let event = create_event_with_geohash("Consistent routing", "u09tu").await;
    
    // Try from multiple connection endpoints
    let endpoints = vec![
        Scope::Default,
        Scope::named("team1").unwrap(),
        Scope::named("admin").unwrap(),
        Scope::named("9q8yy").unwrap(),  // Different geohash
    ];
    
    for endpoint in endpoints {
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(endpoint.clone());
        
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        // All should route to same geohash scope regardless of connection endpoint
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    Scope::Named { name, .. } => {
                        assert_eq!(name, "u09tu", 
                                   "Event should always route to its geohash scope");
                    }
                    _ => panic!("Expected Named scope"),
                }
            }
            _ => panic!("Expected SaveSignedEvent"),
        }
    }
}

#[test]
fn test_scope_comparison() {
    // Verify that different scopes are actually different
    let scope1 = Scope::named("drt2z").unwrap();
    let scope2 = Scope::named("9q8yy").unwrap();
    let scope3 = Scope::Default;
    
    assert_ne!(scope1, scope2, "Different geohash scopes should not be equal");
    assert_ne!(scope1, scope3, "Geohash scope should not equal root scope");
    assert_ne!(scope2, scope3, "Different geohash scope should not equal root scope");
    
    // Same geohash should create equal scopes
    let scope1_copy = Scope::named("drt2z").unwrap();
    assert_eq!(scope1, scope1_copy, "Same geohash should create equal scopes");
}