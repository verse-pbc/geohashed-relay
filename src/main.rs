#![recursion_limit = "256"]

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
        // Debug: Print the type of the base chain (should have RelayMiddleware as innermost)
        let chain_step1 = chain
            .with(RateLimitMiddleware::new(
                Quota::per_minute(NonZeroU32::new(config.events_per_minute).unwrap())
            ));
        
        // At this point, chain is: RateLimitMiddleware -> RelayMiddleware -> End
        let chain_step2 = chain_step1.with(Nip40ExpirationMiddleware);
        // Now: Nip40ExpirationMiddleware -> RateLimitMiddleware -> RelayMiddleware -> End
        
        let chain_step3 = chain_step2.with(ErrorHandlingMiddleware::new());
        // Now: ErrorHandlingMiddleware -> Nip40ExpirationMiddleware -> RateLimitMiddleware -> RelayMiddleware -> End
        
        let final_chain = chain_step3.with(NostrLoggerMiddleware::new());
        // Final: NostrLoggerMiddleware -> ErrorHandlingMiddleware -> Nip40ExpirationMiddleware -> RateLimitMiddleware -> RelayMiddleware -> End
        
        // Print the type name (this will be very long!)
        info!("Middleware chain type: {}", std::any::type_name_of_val(&final_chain));
        
        final_chain
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
            // Extract subdomain from Host header for the info page
            let subdomain = headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .and_then(|host| {
                    // Extract subdomain if it exists
                    let parts: Vec<&str> = host.split('.').collect();
                    if parts.len() > 2 || (parts.len() == 2 && !parts[0].contains(':')) {
                        Some(parts[0].to_string())
                    } else {
                        None
                    }
                });
            
            // Generate informative HTML based on current scope
            let html = generate_info_html(subdomain.as_deref());
            Response::builder()
                .status(200)
                .header("content-type", "text/html; charset=utf-8")
                .body(html.into())
                .unwrap()
        }
    }
}

fn generate_info_html(subdomain: Option<&str>) -> String {
    let (title, heading, badge, accepted_rules, rejected_rules, error_section, usage_examples) = match subdomain {
        Some(sub) if crate::geohash_utils::is_valid_geohash(sub) => {
            (
                format!("Geohash Relay: {}", sub.to_uppercase()),
                format!("Geohash Scope: {}", sub.to_uppercase()),
                format!(r#"<span class="badge geohash">{}</span>"#, sub.to_uppercase()),
                vec![
                    format!(r#"Events with ["g", "{}"] tag"#, sub),
                    "Events without any geohash tag".to_string(),
                ],
                vec!["Events with different geohash tags".to_string()],
                None,
                format!(
                    r#"# ✅ Post event with matching geohash tag
nak event -c "Event for {}" -t g={} wss://{}.hashstr.com

# ✅ Post event without geohash tag
nak event -c "Regular event" wss://{}.hashstr.com

# ❌ Wrong geohash tag (will be rejected)
nak event -c "Wrong tag" -t g=other wss://{}.hashstr.com

# Query events from this geohash scope
nak req -t g={} -l 10 wss://{}.hashstr.com"#,
                    sub, sub, sub, sub, sub, sub, sub
                ),
            )
        }
        Some(sub) => {
            // Invalid subdomain
            (
                "Invalid Subdomain".to_string(),
                "Invalid Subdomain".to_string(),
                r#"<span class="badge error">INVALID</span>"#.to_string(),
                vec![],
                vec![],
                Some(format!(
                    r#"<div class="error-box">
                        <h3>⚠️ Invalid Subdomain</h3>
                        <p>'{}' is not a valid geohash.</p>
                        <p>Only valid geohash strings can be used as subdomains.</p>
                    </div>"#,
                    sub
                )),
                String::new(),
            )
        }
        None => {
            // Root domain
            (
                "Geohashed Relay".to_string(),
                "Geohashed Relay".to_string(),
                r#"<span class="badge root">ROOT</span>"#.to_string(),
                vec!["Events without geohash tags".to_string()],
                vec![
                    r#"Events with ["g", "geohash"] tags"#.to_string(),
                    "Must be posted to matching subdomain".to_string(),
                ],
                None,
                r#"# ✅ Post event without geohash tag
nak event -c "Global announcement" wss://hashstr.com

# ❌ Geotagged event (will be rejected)
nak event -c "SF meetup" -t g=drt2z wss://hashstr.com
# Error: use wss://drt2z.hashstr.com instead

# Query all events from root scope
nak req -l 10 wss://hashstr.com"#.to_string(),
            )
        }
    };

    let accepted_html = if !accepted_rules.is_empty() {
        format!(
            r#"<div class="rule-box accept">
                <h3>✅ Accepted Events</h3>
                <ul>
                    {}
                </ul>
            </div>"#,
            accepted_rules.iter().map(|r| format!("<li>{}</li>", r)).collect::<Vec<_>>().join("\n")
        )
    } else {
        String::new()
    };

    let rejected_html = if !rejected_rules.is_empty() {
        format!(
            r#"<div class="rule-box reject">
                <h3>❌ Rejected Events</h3>
                <ul>
                    {}
                </ul>
            </div>"#,
            rejected_rules.iter().map(|r| format!("<li>{}</li>", r)).collect::<Vec<_>>().join("\n")
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{}</title>
    <style>
        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}
        
        body {{
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
            background: #0f0f23;
            color: #e4e4e7;
            min-height: 100vh;
            padding: 40px 20px;
        }}
        
        .container {{
            max-width: 900px;
            margin: 0 auto;
        }}
        
        h1 {{
            font-size: 2.5rem;
            font-weight: 600;
            margin-bottom: 10px;
            color: #f0f0f0;
        }}
        
        .badge {{
            display: inline-block;
            padding: 6px 12px;
            font-size: 0.85rem;
            font-weight: 600;
            border-radius: 6px;
            margin-left: 12px;
            text-transform: uppercase;
            letter-spacing: 0.5px;
        }}
        
        .badge.root {{
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            color: white;
        }}
        
        .badge.geohash {{
            background: linear-gradient(135deg, #4ade80 0%, #22c55e 100%);
            color: white;
        }}
        
        .badge.error {{
            background: linear-gradient(135deg, #f87171 0%, #dc2626 100%);
            color: white;
        }}
        
        .description {{
            color: #9ca3af;
            font-size: 1.1rem;
            line-height: 1.6;
            margin: 20px 0 40px 0;
            max-width: 800px;
        }}
        
        .section {{
            margin: 40px 0;
        }}
        
        .section-title {{
            color: #60a5fa;
            font-size: 0.9rem;
            font-weight: 600;
            text-transform: uppercase;
            letter-spacing: 1px;
            margin-bottom: 20px;
        }}
        
        .code-block {{
            background: #1a1a2e;
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            padding: 24px;
            overflow-x: auto;
        }}
        
        .code-block pre {{
            margin: 0;
            font-family: 'SF Mono', 'Monaco', 'Inconsolata', 'Fira Code', monospace;
            font-size: 0.95rem;
            line-height: 1.6;
            color: #e4e4e7;
        }}
        
        .comment {{
            color: #6b7280;
        }}
        
        .url {{
            color: #60a5fa;
        }}
        
        .tag {{
            color: #fbbf24;
        }}
        
        .error {{
            color: #f87171;
        }}
        
        .rules {{
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 24px;
            margin: 40px 0;
        }}
        
        @media (max-width: 768px) {{
            .rules {{
                grid-template-columns: 1fr;
            }}
        }}
        
        .rule-box {{
            background: rgba(30, 30, 46, 0.6);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            padding: 20px;
        }}
        
        .rule-box.accept {{
            border-left: 3px solid #4ade80;
        }}
        
        .rule-box.reject {{
            border-left: 3px solid #f87171;
        }}
        
        .rule-box h3 {{
            font-size: 1.1rem;
            font-weight: 600;
            margin-bottom: 12px;
            color: #f0f0f0;
        }}
        
        .rule-box ul {{
            list-style: none;
            padding: 0;
        }}
        
        .rule-box li {{
            padding: 8px 0;
            color: #d1d5db;
            line-height: 1.5;
        }}
        
        .rule-box li:before {{
            content: "• ";
            color: #60a5fa;
            margin-right: 8px;
        }}
        
        .error-box {{
            background: rgba(220, 38, 38, 0.1);
            border: 1px solid rgba(220, 38, 38, 0.3);
            border-radius: 8px;
            padding: 20px;
            margin: 30px 0;
        }}
        
        .error-box h3 {{
            color: #f87171;
            margin-bottom: 10px;
        }}
        
        .error-box p {{
            color: #fca5a5;
            line-height: 1.6;
            margin: 5px 0;
        }}
        
        code {{
            background: rgba(0, 0, 0, 0.4);
            padding: 2px 6px;
            border-radius: 4px;
            font-family: 'SF Mono', 'Monaco', monospace;
            font-size: 0.9rem;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>
            {}
            {}
        </h1>
        
        <p class="description">
            A Nostr relay with enforced geohash-based data isolation. Each geohash subdomain is a completely separate data scope with no hierarchical propagation. Events with geohash tags must be posted to their matching subdomain.
        </p>
        
        {}
        
        <div class="section">
            <div class="section-title">NAK Usage Examples</div>
            <div class="code-block">
                <pre>{}</pre>
            </div>
        </div>
        
        <div class="rules">
            {}
            {}
        </div>
    </div>
</body>
</html>"#,
        title,           // Page <title>
        heading,         // Main heading
        badge,           // Badge (ROOT/GEOHASH/INVALID)
        error_section.unwrap_or_default(),  // Error section if any
        usage_examples,  // Code examples
        accepted_html,   // Accepted rules
        rejected_html    // Rejected rules
    )
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