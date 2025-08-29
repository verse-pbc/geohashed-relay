mod config;
mod processor;
mod geohash_utils;

use anyhow::Result;
use axum::{
    extract::{State as AxumState, ConnectInfo},
    response::Response,
    routing::get,
    Router,
};
use relay_builder::{WebSocketUpgrade, handle_upgrade, HandlerFactory};
use relay_builder::ScopeConfig;
use nostr_sdk::prelude::*;
use relay_builder::{
    RelayBuilder, RelayConfig as BuilderConfig,
    middlewares::{NostrLoggerMiddleware, Nip40ExpirationMiddleware, RateLimitMiddleware, ErrorHandlingMiddleware},
};
use governor::Quota;
use std::{net::SocketAddr, sync::Arc, num::NonZeroU32};
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    cors::CorsLayer,
    trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::{info, warn, Level};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::RelayConfig;
use crate::processor::{ConnectionState, GeohashedEventProcessor};

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables
    dotenv::dotenv().ok();
    
    // Initialize tracing
    init_tracing();
    
    // Load configuration
    let config = RelayConfig::from_env()?;
    info!("Starting Geohashed Relay on {}:{}", config.host, config.port);
    info!("Database path: {}", config.database_path);
    info!("Rate limit: {} events/min", config.events_per_minute);
    
    // Load or generate relay keys
    let keys = if let Ok(private_key_hex) = std::env::var("RELAY_PRIVATE_KEY") {
        // Use provided private key
        match SecretKey::from_hex(&private_key_hex) {
            Ok(secret_key) => {
                let keys = Keys::new(secret_key);
                info!("Using provided relay private key");
                keys
            },
            Err(e) => {
                warn!("Failed to parse RELAY_PRIVATE_KEY: {}. Generating new keys.", e);
                Keys::generate()
            }
        }
    } else {
        // Generate random keys (development/testing)
        warn!("No RELAY_PRIVATE_KEY provided. Generating random keys (not suitable for production!)");
        Keys::generate()
    };
    info!("Relay public key: {}", keys.public_key());
    
    // Create the event processor (rate limiting now handled by middleware)
    let processor = GeohashedEventProcessor::new();
    
    // Configure the relay with subdomain support
    let mut relay_config = BuilderConfig::new(
        &config.relay_url,
        config.database_path.clone(),
        keys.clone(),
    );
    
    // Configure subdomain support - extract subdomains from host header
    relay_config.scope_config = ScopeConfig::Subdomain {
        base_domain_parts: 2, // e.g., "example.com" has 2 parts
    };
    
    // Set limits on the config
    relay_config.max_subscriptions = config.max_subscriptions_per_connection;
    relay_config.max_limit = config.max_limit_per_filter;
    // Note: max_event_size is handled at a different layer
    
    // Build the relay with middleware
    let builder = RelayBuilder::<ConnectionState>::new(relay_config)
        .custom_state::<ConnectionState>()
        .event_processor(processor)
        .without_defaults(); // We'll add middleware manually
    
    // Build with middleware
    info!("Building relay with middleware...");
    if config.enable_nip40_expiration {
        info!("- NIP-40 expiration checking enabled");  
    }
    
    let handler = builder.build_with(|chain| {
        chain
            .with(ErrorHandlingMiddleware::new())  // Add this FIRST to catch all errors
            .with(NostrLoggerMiddleware::new())
            .with(RateLimitMiddleware::new(
                Quota::per_minute(NonZeroU32::new(config.events_per_minute).unwrap())
            ))
            .with(Nip40ExpirationMiddleware)
    }).await?;
    
    // Create the Axum app
    let app = create_app(handler, config.metrics_enabled);
    
    // Start the server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Relay listening on http://{}", addr);
    
    // Start metrics server if enabled
    let metrics_handle = if config.metrics_enabled {
        Some(start_metrics_server(config.metrics_port))
    } else {
        None
    };
    
    // Run the server with graceful shutdown
    let app = app.into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    
    // Wait for metrics server to finish
    if let Some(handle) = metrics_handle {
        let _ = handle.await?;
    }
    
    info!("Relay shutdown complete");
    Ok(())
}

fn create_app(handler: impl HandlerFactory + Send + Sync + 'static, metrics_enabled: bool) -> Router
{
    let handler = Arc::new(handler);
    
    let mut app = Router::new()
        .route("/", get(websocket_handler))
        .route("/health", get(health_check))
        .with_state(handler)
        .layer(
            ServiceBuilder::new()
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                        .on_request(DefaultOnRequest::new().level(Level::DEBUG))
                        .on_response(DefaultOnResponse::new().level(Level::DEBUG)),
                )
                .layer(CorsLayer::permissive()),
        );
    
    if metrics_enabled {
        app = app.route("/metrics", get(metrics_handler));
    }
    
    app
}

async fn websocket_handler<H>(
    ws: Option<WebSocketUpgrade>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    AxumState(handler): AxumState<Arc<H>>,
) -> Response
where
    H: HandlerFactory + Send + Sync + 'static,
{
    match ws {
        Some(ws) => {
            let h = handler.create(&headers);
            handle_upgrade(ws, addr, h).await
        },
        None => {
            // Return relay info for NIP-11 if requested
            Response::builder()
                .status(200)
                .body("Scoped Relay".into())
                .unwrap()
        }
    }
}

async fn health_check() -> &'static str {
    "OK"
}

async fn metrics_handler() -> String {
    // Placeholder for metrics - you can integrate with metrics crate here
    "# Metrics endpoint\n# Add prometheus metrics here\n".to_string()
}

fn start_metrics_server(port: u16) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let app = Router::new()
            .route("/metrics", get(metrics_handler));
        
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!("Metrics server listening on http://{}", addr);
        
        axum::serve(listener, app).await?;
        Ok(())
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C, starting graceful shutdown");
        },
        _ = terminate => {
            info!("Received terminate signal, starting graceful shutdown");
        },
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,scoped_relay=debug,relay_builder=debug"));
    
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(true)
        .with_line_number(true);
    
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}