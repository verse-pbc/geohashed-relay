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
            // Extract subdomain and domain from Host header for the info page
            let host_str = headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost");
            
            let parts: Vec<&str> = host_str.split('.').collect();
            let (subdomain, domain) = if parts.len() > 2 || (parts.len() == 2 && !parts[0].contains(':')) {
                // Has subdomain
                let sub = parts[0].to_string();
                let dom = parts[1..].join(".");
                (Some(sub), dom)
            } else {
                // No subdomain, just domain
                (None, host_str.to_string())
            };
            
            // Generate informative HTML based on current scope
            let html = generate_info_html(subdomain.as_deref(), &domain);
            Response::builder()
                .status(200)
                .header("content-type", "text/html; charset=utf-8")
                .body(html.into())
                .unwrap()
        }
    }
}

fn generate_info_html(subdomain: Option<&str>, domain: &str) -> String {
    // Common Nostr event kinds that use geohash tags:
    // - Kind 20000: Ephemeral geohash events (location-based messages, e.g., BitChat)
    // - Kind 1: Text notes (regular posts with optional location tagging)
    // - Kind 0: Metadata (profiles with location, rare)
    
    // Generate map HTML for geohash subdomains with clickable grid
    let map_section = subdomain.and_then(|sub| {
        if crate::geohash_utils::is_valid_geohash(sub) {
            // Get center coordinates and precision
            let center_decoded = geohash::decode(sub).ok()?;
            let precision = sub.len();
            
            // Calculate zoom level
            let zoom = match precision {
                1 => 2,
                2 => 4,
                3 => 7,
                4 => 10,
                5 => 12,
                6 => 14,
                7 => 18,
                _ => 16,
            };
            
            Some(format!(
                r#"<div class="section">
                    <div class="section-title">Geohash Grid Map</div>
                    <div id="map" style="height: 400px; border-radius: 8px; border: 1px solid rgba(255, 255, 255, 0.1);"></div>
                    <link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" />
                    <script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
                    <script>
                        // Polyfill for module to avoid error
                        if (typeof module === 'undefined') {{
                            window.module = {{ exports: {{}} }};
                        }}
                    </script>
                    <script src="https://cdn.jsdelivr.net/npm/ngeohash@0.6.3/main.js"></script>
                    <script>
                        var map = L.map('map').setView([{}, {}], {});
                        L.tileLayer('https://{{s}}.tile.openstreetmap.org/{{z}}/{{x}}/{{y}}.png', {{
                            attribution: '© OpenStreetMap contributors'
                        }}).addTo(map);
                        
                        var currentGeohash = '{}';
                        var geohashLayer = null;
                        
                        function generateGeohashGrid() {{
                            if (geohashLayer) {{
                                map.removeLayer(geohashLayer);
                            }}
                            
                            var bounds = map.getBounds();
                            var zoom = map.getZoom();
                            
                            // Determine precision based on zoom level
                            // Adjust precision dynamically based on zoom to avoid rendering issues
                            var precision;
                            
                            // Calculate precision based on zoom level
                            // Lower zoom = lower precision (coarse grid)
                            // Higher zoom = higher precision (fine grid)
                            if (zoom < 3) precision = 1;
                            else if (zoom < 6) precision = 2;
                            else if (zoom < 9) precision = 3;
                            else if (zoom < 12) precision = 4;
                            else if (zoom < 15) precision = 5;
                            else if (zoom < 18) precision = 6;
                            else precision = 7;
                            
                            // Ensure we don't exceed max precision
                            precision = Math.min(precision, 7);
                            
                            // Get all geohashes that intersect with the visible area
                            var geohashSet = new Set();
                            
                            // Get corner geohashes
                            var sw = geohash.encode(bounds.getSouth(), bounds.getWest(), precision);
                            var ne = geohash.encode(bounds.getNorth(), bounds.getEast(), precision);
                            
                            // Decode to get the actual bounds of these geohashes
                            var swBounds = geohash.decode_bbox(sw);
                            var neBounds = geohash.decode_bbox(ne);
                            
                            // Calculate how many geohash cells we need to cover
                            var cellSize = swBounds[3] - swBounds[1]; // longitude width of one cell
                            var cellHeight = swBounds[2] - swBounds[0]; // latitude height of one cell
                            
                            // Generate all geohashes in the grid
                            // Limit total cells to prevent performance issues
                            var maxCells = 200;
                            var cellCount = 0;
                            
                            for (var lat = swBounds[0]; lat <= neBounds[2] + cellHeight && cellCount < maxCells; lat += cellHeight * 0.99) {{
                                for (var lng = swBounds[1]; lng <= neBounds[3] + cellSize && cellCount < maxCells; lng += cellSize * 0.99) {{
                                    var gh = geohash.encode(lat, lng, precision);
                                    if (gh) {{
                                        var ghBounds = geohash.decode_bbox(gh);
                                        // Check if this geohash intersects with the viewport
                                        if (ghBounds[2] >= bounds.getSouth() && ghBounds[0] <= bounds.getNorth() &&
                                            ghBounds[3] >= bounds.getWest() && ghBounds[1] <= bounds.getEast()) {{
                                            geohashSet.add(gh);
                                            cellCount++;
                                        }}
                                    }}
                                }}
                            }}
                            
                            // Create GeoJSON features
                            var features = [];
                            geohashSet.forEach(function(gh) {{
                                var bbox = geohash.decode_bbox(gh);
                                // bbox is [minlat, minlon, maxlat, maxlon]
                                features.push({{
                                    type: 'Feature',
                                    properties: {{
                                        geohash: gh,
                                        isCenter: gh === currentGeohash
                                    }},
                                    geometry: {{
                                        type: 'Polygon',
                                        coordinates: [[
                                            [bbox[1], bbox[0]],  // SW: minlon, minlat
                                            [bbox[3], bbox[0]],  // SE: maxlon, minlat
                                            [bbox[3], bbox[2]],  // NE: maxlon, maxlat
                                            [bbox[1], bbox[2]],  // NW: minlon, maxlat
                                            [bbox[1], bbox[0]]   // close polygon
                                        ]]
                                    }}
                                }});
                            }});
                            
                            // Add layer to map
                            geohashLayer = L.geoJSON({{
                                type: 'FeatureCollection',
                                features: features
                            }}, {{
                                style: function(feature) {{
                                    if (feature.properties.isCenter) {{
                                        return {{
                                            fillColor: '#4ade80',
                                            weight: 2,
                                            opacity: 1,
                                            color: '#4ade80',
                                            fillOpacity: 0.3
                                        }};
                                    }} else {{
                                        return {{
                                            fillColor: '#60a5fa',
                                            weight: 0.5,
                                            opacity: 0.7,
                                            color: '#60a5fa',
                                            fillOpacity: 0.05
                                        }};
                                    }}
                                }},
                                onEachFeature: function(feature, layer) {{
                                    var gh = feature.properties.geohash;
                                    var isCenter = feature.properties.isCenter;
                                    
                                    // Add permanent label for all cells
                                    layer.bindTooltip(gh, {{
                                        permanent: true,
                                        direction: 'center',
                                        className: isCenter ? 'geohash-label-center' : 'geohash-label'
                                    }});
                                    
                                    // Make clickable - always navigate to subdomain
                                    layer.on('click', function(e) {{
                                        if (!isCenter) {{
                                            window.location.href = 'https://' + gh + '.{}';
                                        }}
                                    }});
                                    
                                    // Add hover effects
                                    if (!isCenter) {{
                                        layer.on('mouseover', function(e) {{
                                            this.setStyle({{
                                                fillOpacity: 0.2,
                                                weight: 1.5
                                            }});
                                        }});
                                        
                                        layer.on('mouseout', function(e) {{
                                            this.setStyle({{
                                                fillOpacity: 0.05,
                                                weight: 0.5
                                            }});
                                        }});
                                    }}
                                }}
                            }}).addTo(map);
                        }}
                        
                        // Generate initial grid
                        generateGeohashGrid();
                        
                        // Regenerate on map move/zoom
                        map.on('moveend', function() {{
                            generateGeohashGrid();
                        }});
                    </script>
                    <style>
                        .geohash-label {{
                            background: rgba(96, 165, 250, 0.9);
                            border: none;
                            color: white;
                            font-weight: 600;
                            font-size: 10px;
                            padding: 1px 4px;
                            white-space: nowrap;
                        }}
                        .geohash-label-center {{
                            background: #4ade80;
                            border: none;
                            color: white;
                            font-weight: bold;
                            font-size: 12px;
                            padding: 3px 8px;
                            box-shadow: 0 2px 4px rgba(0,0,0,0.3);
                            white-space: nowrap;
                        }}
                        .leaflet-interactive:hover {{
                            cursor: pointer;
                        }}
                    </style>
                </div>"#,
                center_decoded.0.y, center_decoded.0.x, zoom,
                sub,
                domain
            ))
        } else {
            None
        }
    }).unwrap_or_default();
    
    let (title, heading, badge, description, accepted_rules, rejected_rules, error_section, usage_examples) = match subdomain {
        Some(sub) if crate::geohash_utils::is_valid_geohash(sub) => {
            (
                format!("{} Nostr Relay", sub),
                format!(r#"Nostr Relay <span style="color: #4ade80; font-weight: 600;">[{}]</span>"#, sub),
                String::new(),  // No badge
                format!(r#"<div style="line-height: 1.8;">
                    <p style="margin-bottom: 16px;">Each geohash subdomain (e.g., <code style="background: rgba(74, 222, 128, 0.1); padding: 2px 6px; border-radius: 4px; color: #4ade80;">{}.{}</code>) represents a distinct geographic cell with enforced data isolation.</p>
                    <ul style="list-style: none; padding-left: 0; margin: 0;">
                        <li style="margin-bottom: 12px; padding-left: 24px; position: relative;">
                            <span style="position: absolute; left: 0; color: #4ade80;">•</span>
                            Events explicitly tagged with <code style="background: rgba(74, 222, 128, 0.1); padding: 2px 6px; border-radius: 4px; color: #4ade80;">["g", "{}"]</code> must be posted here
                        </li>
                        <li style="margin-bottom: 12px; padding-left: 24px; position: relative;">
                            <span style="position: absolute; left: 0; color: #4ade80;">•</span>
                            Events without geohash tags posted here are implicitly bound to the <strong>{}</strong> location — by choosing this endpoint, publishers signal that these events belong to this geographic scope, even without explicit tags
                        </li>
                        <li style="padding-left: 24px; position: relative;">
                            <span style="position: absolute; left: 0; color: #4ade80;">•</span>
                            <strong>Cells are isolated:</strong> there is no hierarchy across geohash levels. For example, events in <code style="background: rgba(74, 222, 128, 0.1); padding: 2px 6px; border-radius: 4px; color: #4ade80;">{}a</code> are not visible in <code style="background: rgba(74, 222, 128, 0.1); padding: 2px 6px; border-radius: 4px; color: #4ade80;">{}</code>, and vice versa. Think of each subdomain as a separate room in a building — conversations stay in the room they were spoken, and don't leak into adjacent or larger spaces
                        </li>
                    </ul>
                </div>"#, sub, domain, sub, sub, sub, sub),
                vec![
                    format!(r#"Events with ["g", "{}"] tag"#, sub),
                    "Events without any geohash tag".to_string(),
                ],
                vec!["Events with different geohash tags".to_string()],
                None::<String>,
                format!(
                    r#"<span class="comment"># Post location-based message (ephemeral)</span>
nak event -k 20000 -c "Hello from {}!" -t g={} wss://{}.{}

<span class="comment"># Post event without geohash tag</span>
nak event -c "Regular event" wss://{}.{}

<span class="comment"># Wrong geohash tag (will be rejected)</span>
nak event -c "Wrong tag" -t g=other wss://{}.{}

<span class="comment"># Query events from this geohash scope</span>
nak req -l 10 wss://{}.{}"#,
                    sub, sub, sub, domain, sub, domain, sub, domain, sub, domain
                ),
            )
        }
        Some(sub) => {
            // Invalid subdomain - show as root relay with note
            (
                format!("Nostr Relay"),
                format!("Nostr Relay"),
                String::new(),
                format!("A Nostr relay with geohash-based data isolation. Note: '{}' is not a valid geohash subdomain.", sub),
                vec!["Events without geohash tags".to_string()],
                vec![
                    r#"Events with ["g", "geohash"] tags"#.to_string(),
                    "Must be posted to matching subdomain".to_string(),
                ],
                None::<String>,
                format!(r#"<span class="comment"># Post event without geohash tag</span>
nak event -c "Global announcement" wss://{}

<span class="comment"># Location event (requires valid geohash subdomain)</span>
nak event -k 20000 -c "SF meetup" -t g=drt2z wss://{}
<span class="comment"># Error: use wss://drt2z.{} instead</span>

<span class="comment"># Query all events from root scope</span>
nak req -l 10 wss://{}"#,
                    domain, domain, domain, domain
                )
            )
        }
        None => {
            // Root domain
            (
                format!("Nostr Relay"),
                format!("Nostr Relay"),
                String::new(),  // No badge
                "A Nostr relay with geohash-based data isolation. Events with geohash tags must be posted to their matching subdomain.".to_string(),
                vec!["Events without geohash tags".to_string()],
                vec![
                    r#"Events with ["g", "geohash"] tags"#.to_string(),
                    "Must be posted to matching subdomain".to_string(),
                ],
                None::<String>,
                format!(r#"<span class="comment"># Post event without geohash tag</span>
nak event -c "Global announcement" wss://{}

<span class="comment"># Location event (will be rejected - wrong subdomain)</span>
nak event -k 20000 -c "SF meetup" -t g=drt2z wss://{}
<span class="comment"># Error: use wss://drt2z.{} instead</span>

<span class="comment"># Geotagged note (will be rejected - wrong subdomain)</span>
nak event -k 1 -c "Beach photo" -t g=9q8yy wss://{}
<span class="comment"># Error: use wss://9q8yy.{} instead</span>

<span class="comment"># Query all events from root scope</span>
nak req -l 10 wss://{}"#,
                    domain, domain, domain, domain, domain, domain
                ),
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
            background: #0a0a0f;
            border: 1px solid rgba(255, 255, 255, 0.08);
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
            color: #4b5563;
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
            {}
        </p>
        
        {}
        
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
        description,     // Description of the relay behavior
        error_section.unwrap_or_default(),  // Error section if any
        map_section,     // Map visualization for geohash
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