#[cfg(test)]
mod tests {
    use super::super::*;

    fn create_test_processor() -> GeohashedEventProcessor {
        GeohashedEventProcessor::new()
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

    fn create_test_context(subdomain: nostr_lmdb::Scope) -> EventContext {
        let keys = Keys::generate();
        EventContext {
            relay_pubkey: keys.public_key(),
            subdomain: Arc::new(subdomain),
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
        let result = processor.handle_event(event.clone(), state, &context).await;
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
        let result = processor.handle_event(event.clone(), state, &context).await;
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
        
        // Create context for a valid geohash subdomain connection
        let subdomain_scope = nostr_lmdb::Scope::named("gbsuv").unwrap(); // London geohash
        let context = create_test_context(subdomain_scope.clone());
        
        // Process event
        let result = processor.handle_event(event.clone(), state, &context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        assert_eq!(commands.len(), 1);
        
        // Verify event stays in subdomain scope
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "gbsuv");
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
        let result = processor.handle_event(event.clone(), state, &context).await;
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
        let result = processor.handle_event(event.clone(), state, &context).await;
        assert!(result.is_err());
        
        // Verify error message guides to correct subdomain
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(error_msg.contains("restricted"));
            assert!(error_msg.contains("events with geohash 'drt2z' must be posted to wss://drt2z.hashstr.com"));
        }
    }
    

    #[tokio::test]
    async fn test_invalid_subdomain_rejected() {
        let processor = create_test_processor();
        let event = create_event_without_geohash().await;
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        
        // Try to post to an invalid subdomain (not a valid geohash)
        let invalid_scope = nostr_lmdb::Scope::named("foobar").unwrap();
        let context = create_test_context(invalid_scope);
        
        // Should reject the event
        let result = processor.handle_event(event.clone(), state, &context).await;
        assert!(result.is_err());
        
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(error_msg.contains("'foobar' is not a valid geohash subdomain"));
        }
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
        
        // Connect via a valid geohash subdomain (not "team1" which is invalid)
        let subdomain_scope = nostr_lmdb::Scope::named("u09tu").unwrap();
        let context = create_test_context(subdomain_scope.clone());
        
        // Process event
        let result = processor.handle_event(event.clone(), state, &context).await;
        assert!(result.is_ok());
        
        let commands = result.unwrap();
        
        // Invalid geohash should be ignored, event stored in connection's scope
        match &commands[0] {
            StoreCommand::SaveSignedEvent(_, scope, _) => {
                match scope {
                    nostr_lmdb::Scope::Named { name, .. } => {
                        assert_eq!(name, "u09tu", "Invalid geohash ignored, uses subdomain");
                    }
                    _ => panic!("Expected Named scope"),
                }
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }
}