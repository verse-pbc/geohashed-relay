# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Development Commands

### Building and Running
```bash
# Development build
cargo build

# Production build (optimized)
cargo build --release

# Run development server
cargo run

# Run with custom configuration
RELAY_PORT=3000 cargo run --release
```

### Testing
```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run integration tests only
cargo test --test '*'
```

### Code Quality
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Strict clippy check
cargo clippy --all-targets --all-features -- -D warnings
```

## High-Level Architecture

This is a **geohashed Nostr relay** built on the `relay_builder` framework with location-aware routing capabilities:

### Core Components

1. **Multi-Tenant Isolation**: Complete data isolation by subdomain using `nostr_lmdb::Scope`. Events posted to different subdomains are stored in separate database partitions.

2. **Geohash Routing**: Events with geohash tags (`["g", "geohash"]`) are automatically routed to their corresponding geohash scope, regardless of the endpoint they're posted to. The routing logic is in `src/processor.rs`.

3. **Event Processing Pipeline**:
   - `GeohashedEventProcessor` (src/processor.rs) - Main event handler with rate limiting and geohash extraction
   - `geohash_utils` (src/geohash_utils.rs) - Geohash validation and extraction utilities
   - Events flow through middleware chain: NostrLogger → Nip42Auth → Nip40Expiration → Processor

4. **WebSocket Handler**: Built on `websocket_builder` and `axum`, handles upgrade and connection management.

5. **Storage**: Uses LMDB with scope-based partitioning. Each subdomain/geohash gets its own isolated storage space.

### Key Design Patterns

- **Exact Matching Only**: Geohash queries match only at the exact precision level (no hierarchical propagation)
- **Auto-Forwarding**: Events with geohash tags override subdomain routing
- **Rate Limiting**: Per-connection rate limiting using fixed-window counters
- **Middleware Chain**: Composable middleware for auth, logging, and expiration

### Dependencies

- **relay_builder**: Core relay framework (local path: ../groups/relay_repos/relay_builder)
- **websocket_builder**: WebSocket handling (local path: ../groups/relay_repos/websocket_builder)
- **nostr-sdk/nostr/nostr-lmdb**: Nostr protocol and storage (from verse-pbc/nostr fork)
- **axum**: Web framework for HTTP/WebSocket endpoints
- **geohash**: Geohash encoding/decoding library

### Configuration

Configuration via environment variables (`.env` file):
- Server: `RELAY_HOST`, `RELAY_PORT`, `RELAY_URL`
- Database: `DATABASE_PATH`
- Limits: `MAX_SUBSCRIPTIONS_PER_CONNECTION`, `EVENTS_PER_MINUTE`
- Multi-tenancy: `ALLOWED_SUBDOMAINS` (comma-separated whitelist)
- Auth: `REQUIRE_AUTH_FOR_WRITE`, `REQUIRE_AUTH_FOR_READ`

### Deployment

- Docker support with multi-stage build (see Dockerfile)
- Kubernetes deployment via Helm chart in `deployment/geohashed-relay/`
- GitHub Actions for CI/CD in `.github/workflows/publish-and-release.yml`