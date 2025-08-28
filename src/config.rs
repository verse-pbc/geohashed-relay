use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    // Server settings
    pub host: String,
    pub port: u16,
    pub relay_url: String,
    
    // Database
    pub database_path: String,
    
    // Limits
    pub max_event_size: usize,
    pub max_subscriptions_per_connection: usize,
    pub max_filters_per_subscription: usize,
    pub max_limit_per_filter: usize,
    
    // Multi-tenancy
    pub allowed_subdomains: HashSet<String>,
    
    // Rate limiting
    pub events_per_minute: u32,
    
    // Features
    pub require_auth_for_write: bool,
    pub require_auth_for_read: bool,
    pub enable_nip42_auth: bool,
    pub enable_nip40_expiration: bool,
    
    // Monitoring
    pub metrics_enabled: bool,
    pub metrics_port: u16,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            relay_url: "ws://localhost:8080".to_string(),
            database_path: "./data".to_string(),
            max_event_size: 128 * 1024, // 128KB
            max_subscriptions_per_connection: 20,
            max_filters_per_subscription: 10,
            max_limit_per_filter: 5000,
            allowed_subdomains: HashSet::new(),
            events_per_minute: 30,  // 0.5 per second - reasonable for normal chat
            require_auth_for_write: false,
            require_auth_for_read: false,
            enable_nip42_auth: true,
            enable_nip40_expiration: true,
            metrics_enabled: true,
            metrics_port: 9090,
        }
    }
}

impl RelayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::default();
        
        if let Ok(host) = std::env::var("RELAY_HOST") {
            config.host = host;
        }
        
        if let Ok(port) = std::env::var("RELAY_PORT") {
            config.port = port.parse()?;
        }
        
        if let Ok(url) = std::env::var("RELAY_URL") {
            config.relay_url = url;
        }
        
        if let Ok(path) = std::env::var("DATABASE_PATH") {
            config.database_path = path;
        }
        
        if let Ok(size) = std::env::var("MAX_EVENT_SIZE") {
            config.max_event_size = size.parse()?;
        }
        
        if let Ok(subs) = std::env::var("ALLOWED_SUBDOMAINS") {
            config.allowed_subdomains = subs.split(',').map(|s| s.trim().to_string()).collect();
        }
        
        if let Ok(rate) = std::env::var("EVENTS_PER_MINUTE") {
            config.events_per_minute = rate.parse()?;
        }
        
        if let Ok(auth) = std::env::var("REQUIRE_AUTH_FOR_WRITE") {
            config.require_auth_for_write = auth.parse()?;
        }
        
        if let Ok(auth) = std::env::var("REQUIRE_AUTH_FOR_READ") {
            config.require_auth_for_read = auth.parse()?;
        }
        
        Ok(config)
    }
}