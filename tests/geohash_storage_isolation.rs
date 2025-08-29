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
    GeohashedEventProcessor::new()
}

/// Helper to create an EventContext
fn create_context(scope: Scope) -> EventContext {
    let keys = Keys::generate();
    EventContext {
        relay_pubkey: keys.public_key(),
        subdomain: Arc::new(scope),
        authed_pubkey: None,
    }
}

#[tokio::test]
async fn test_geohash_rejection_and_acceptance() {
    let processor = create_test_processor();
    
    // Test 1: Event with geohash posted to root domain - should be rejected
    let event_with_geo = create_event_with_geohash("SF event", "drt2z").await;
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let root_context = create_context(Scope::Default);
    
    let result = processor.handle_event(event_with_geo.clone(), state.clone(), &root_context).await;
    assert!(result.is_err(), "Geotagged event should be rejected at root");
    
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("root relay does not accept geotagged events"));
        assert!(msg.contains("drt2z.hashstr.com"));
    }
    
    // Test 2: Same event posted from a different valid geohash subdomain - should be rejected
    let other_geohash_context = create_context(Scope::named("9q8yy").unwrap()); // LA geohash
    let result2 = processor.handle_event(event_with_geo.clone(), Arc::new(RwLock::new(ConnectionState::default())), &other_geohash_context).await;
    assert!(result2.is_err(), "Event should be rejected on non-matching subdomain");
    
    if let Err(e) = result2 {
        let msg = e.to_string();
        assert!(msg.contains("events with geohash 'drt2z' must be posted to"));
        assert!(msg.contains("drt2z.hashstr.com"));
    }
    
    // Test 3: Event posted to matching subdomain - should be accepted and stored
    let matching_context = create_context(Scope::named("drt2z").unwrap());
    let result3 = processor.handle_event(event_with_geo.clone(), Arc::new(RwLock::new(ConnectionState::default())), &matching_context).await;
    assert!(result3.is_ok(), "Event should be accepted on matching subdomain");
    
    let commands = result3.unwrap();
    assert_eq!(commands.len(), 1);
    
    // Verify it's stored in the correct scope
    match &commands[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "drt2z", "Event should be stored in matching geohash scope");
                }
                _ => panic!("Expected Named scope for geohash"),
            }
        }
        _ => panic!("Expected SaveSignedEvent"),
    }
}

#[tokio::test]
async fn test_events_without_geohash_stay_in_connection_scope() {
    let processor = create_test_processor();
    let event = create_regular_event("Regular event").await;
    
    // Test from different connection scopes (only valid geohashes allowed as subdomains)
    let test_cases = vec![
        (Scope::Default, "root"),
        (Scope::named("gbsuv").unwrap(), "gbsuv"),  // London geohash
        (Scope::named("drt2z").unwrap(), "drt2z"),  // SF geohash
    ];
    
    for (connection_scope, expected_name) in test_cases {
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(connection_scope.clone());
        
        let result = processor.handle_event(event.clone(), state, &context).await;
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
    
    // Test 1: Posted to root domain - should be rejected (uses first geohash)
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let context = create_context(Scope::Default);
    
    let result = processor.handle_event(event.clone(), state, &context).await;
    assert!(result.is_err(), "Event with geohash should be rejected at root");
    
    // Test 2: Posted to matching first geohash subdomain - should succeed
    let state2 = Arc::new(RwLock::new(ConnectionState::default()));
    let context2 = create_context(Scope::named("drt2z").unwrap());
    
    let result2 = processor.handle_event(event.clone(), state2, &context2).await;
    assert!(result2.is_ok(), "Event should be accepted on first geohash subdomain");
    
    // Test 3: Posted to second geohash subdomain - should be rejected
    let state3 = Arc::new(RwLock::new(ConnectionState::default()));
    let context3 = create_context(Scope::named("9q8yy").unwrap());
    
    let result3 = processor.handle_event(event, state3, &context3).await;
    assert!(result3.is_err(), "Event should be rejected on non-first geohash subdomain");
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
    
    // Test from valid geohash subdomain
    let state = Arc::new(RwLock::new(ConnectionState::default()));
    let geohash_scope = Scope::named("u09tu").unwrap(); // Valid geohash
    let context = create_context(geohash_scope);
    
    let result = processor.handle_event(event, state, &context).await;
    assert!(result.is_ok());
    
    let commands = result.unwrap();
    
    // Should fall back to connection scope (u09tu) since geohash tag is invalid
    match &commands[0] {
        StoreCommand::SaveSignedEvent(_, scope, _) => {
            match scope {
                Scope::Named { name, .. } => {
                    assert_eq!(name, "u09tu", 
                               "Invalid geohash tag should fall back to connection scope");
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
    
    // Test that each event is rejected from root but accepted on correct subdomain
    for (event, expected_geohash) in events {
        // Test 1: Rejected at root
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(Scope::Default);
        
        let result = processor.handle_event(event.clone(), state, &context).await;
        assert!(result.is_err(), "Geotagged event should be rejected at root");
        
        // Test 2: Accepted at correct subdomain
        let state2 = Arc::new(RwLock::new(ConnectionState::default()));
        let context2 = create_context(Scope::named(expected_geohash).unwrap());
        
        let result2 = processor.handle_event(event.clone(), state2, &context2).await;
        assert!(result2.is_ok(), "Event should be accepted on matching subdomain");
        
        let commands = result2.unwrap();
        match &commands[0] {
            StoreCommand::SaveSignedEvent(stored_event, scope, _) => {
                assert_eq!(stored_event.id, event.id);
                match scope {
                    Scope::Named { name, .. } => {
                        assert_eq!(name, expected_geohash, 
                                   "Event should be stored in its matching scope");
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
    
    // Try from multiple connection endpoints - all should reject except matching one
    let endpoints = vec![
        (Scope::Default, false, "root"),
        (Scope::named("team1").unwrap(), false, "team1"),
        (Scope::named("admin").unwrap(), false, "admin"),
        (Scope::named("9q8yy").unwrap(), false, "different geohash"),
        (Scope::named("u09tu").unwrap(), true, "matching geohash"),  // Only this should accept
    ];
    
    for (endpoint, should_succeed, description) in endpoints {
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let context = create_context(endpoint.clone());
        
        let result = processor.handle_event(event.clone(), state, &context).await;
        
        if should_succeed {
            assert!(result.is_ok(), "Event should be accepted on {}", description);
            let commands = result.unwrap();
            match &commands[0] {
                StoreCommand::SaveSignedEvent(_, scope, _) => {
                    match scope {
                        Scope::Named { name, .. } => {
                            assert_eq!(name, "u09tu", "Event should be stored in matching scope");
                        }
                        _ => panic!("Expected Named scope"),
                    }
                }
                _ => panic!("Expected SaveSignedEvent"),
            }
        } else {
            assert!(result.is_err(), "Event should be rejected on {}", description);
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