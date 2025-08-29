#[cfg(test)]
mod tests {
    use super::super::*;

    fn create_test_processor() -> GeohashedEventProcessor {
        GeohashedEventProcessor::new(
            100,                    // events_per_minute
        )
    }

    async fn create_event_with_geohash(geohash: &str) -> Event {
        let keys = Keys::generate();
        EventBuilder::text_note("Test event")
            .tags(vec![
                Tag::custom(TagKind::Custom("g".into()), vec![geohash.to_string()])
            ])
            .sign(&keys)
            .await
            .unwrap()
    }

    async fn create_event_without_geohash() -> Event {
        let keys = Keys::generate();
        EventBuilder::text_note("Test event without geohash")
            .sign(&keys)
            .await
            .unwrap()
    }

    fn create_test_context(subdomain: nostr_lmdb::Scope) -> EventContext<'static> {
        let keys = Keys::generate();
        let relay_pubkey = Box::leak(Box::new(keys.public_key()));
        let subdomain_ref = Box::leak(Box::new(subdomain));
        EventContext {
            relay_pubkey,
            subdomain: subdomain_ref,
            authed_pubkey: None,
        }
    }

    #[tokio::test]
    async fn test_geohash_rejected_at_root() {
        let processor = create_test_processor();
        let event = create_event_with_geohash("drt2z").await;
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Create context for root domain connection
        let context = create_test_context(nostr_lmdb::Scope::Default);
        
        // Process event - should return error since root doesn't accept geotagged events
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_err());
        
        // Verify error message
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(error_msg.contains("restricted"));
            assert!(error_msg.contains("root relay does not accept geotagged events"));
            assert!(error_msg.contains("drt2z.hashstr.com"));
        }
    }
    
    #[tokio::test]
    async fn test_geohash_correct_scope_stores() {
        let processor = create_test_processor();
        let event = create_event_with_geohash("drt2z").await;
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Create context for matching geohash subdomain
        let context = create_test_context(nostr_lmdb::Scope::named("drt2z").unwrap());
        
        // Process event - should store since we're on the correct subdomain
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        assert_eq!(commands.len(), 1);
        
        // Verify event is stored in correct scope
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "drt2z");
                    }
                    _ => panic!("Expected Named scope with geohash"),
                }
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }

    #[tokio::test]
    async fn test_event_without_geohash_uses_subdomain() {
        let processor = create_test_processor();
        let event = create_event_without_geohash().await;
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Create context for subdomain connection
        let subdomain_scope = nostr_lmdb::Scope::named("team1").unwrap();
        let context = create_test_context(subdomain_scope.clone());
        
        // Process event
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        assert_eq!(commands.len(), 1);
        
        // Verify event stays in subdomain scope
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "team1");
                    }
                    _ => panic!("Expected Named scope with subdomain"),
                }
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }

    #[tokio::test]
    async fn test_multiple_geohash_tags_uses_first() {
        let processor = create_test_processor();
        let keys = Keys::generate();
        let event = EventBuilder::text_note("Test with multiple geohashes")
            .tags(vec![
                Tag::custom(TagKind::Custom("g".into()), vec!["drt2z".to_string()]),
                Tag::custom(TagKind::Custom("g".into()), vec!["9q8yy".to_string()]),
            ])
            .sign(&keys)
            .await
            .unwrap();
        
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        // Use the correct geohash subdomain for the first tag
        let context = create_test_context(nostr_lmdb::Scope::named("drt2z").unwrap());
        
        // Process event
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        // Verify first geohash is used
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "drt2z", "Should use first valid geohash");
                    }
                    _ => panic!("Expected Named scope with geohash"),
                }
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }

    #[tokio::test]
    async fn test_wrong_geohash_subdomain_rejected() {
        let processor = create_test_processor();
        let event = create_event_with_geohash("drt2z").await;
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Create context for different geohash subdomain
        let context = create_test_context(nostr_lmdb::Scope::named("9q8yy").unwrap());
        
        // Process event - should return error since wrong subdomain
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_err());
        
        // Verify error message guides to correct subdomain
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(error_msg.contains("restricted"));
            assert!(error_msg.contains("events with geohash 'drt2z' must be posted to wss://drt2z.hashstr.com"));
        }
    }
    
    #[test]
    fn test_uniform_rate_limiting() {
        let processor = GeohashedEventProcessor::new(
            100,  // rate for all scopes
        );
        
        // All scopes get the same rate
        let geohash_scope = nostr_lmdb::Scope::named("drt2z").unwrap();
        assert_eq!(processor.get_rate_limit(&geohash_scope), 100);
        
        let regular_scope = nostr_lmdb::Scope::named("team1").unwrap();
        assert_eq!(processor.get_rate_limit(&regular_scope), 100);
        
        assert_eq!(processor.get_rate_limit(&nostr_lmdb::Scope::Default), 100);
    }

    #[tokio::test]
    async fn test_invalid_geohash_tag_ignored() {
        let processor = create_test_processor();
        let keys = Keys::generate();
        let event = EventBuilder::text_note("Test with invalid geohash")
            .tags(vec![
                Tag::custom(TagKind::Custom("g".into()), vec!["invalid!".to_string()]),
            ])
            .sign(&keys)
            .await
            .unwrap();
        
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Connect via team1 subdomain
        let subdomain_scope = nostr_lmdb::Scope::named("team1").unwrap();
        let context = create_test_context(subdomain_scope.clone());
        
        // Process event
        let result = processor.handle_event(event.clone(), state, context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        // Invalid geohash should be ignored, event stored in connection's scope
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "team1", "Invalid geohash ignored, uses subdomain");
                    }
                    _ => panic!("Expected Named scope"),
                }
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }
}