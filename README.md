# Geohashed Relay

A Nostr relay with geohash-based location routing and multi-tenant subdomain isolation.

## Features

- **Geohash Routing**: Events with `["g", "geohash"]` tags auto-route to location-specific scopes
- **Multi-tenancy**: Complete data isolation by subdomain
- **Rate limiting**: 30 events/minute per connection (configurable)
- **NIP Support**: NIP-40 (expiration), NIP-42 (auth)

## Quick Start

```bash
# Copy configuration
cp .env.example .env

# Run the relay
cargo run --release
```

The relay will start on `http://localhost:8080`

## How Geohash Routing Works

Events with a geohash tag are automatically routed to their geographic scope:

```javascript
// Post this event to ANY endpoint (e.g., ws://example.com)
const event = {
  kind: 1,
  content: "Coffee meetup at Blue Bottle!",
  tags: [["g", "9q8yy"]]  // San Francisco
}

// Response: ["OK", event_id, true, ""]  ✅ Accepted
// Event is stored ONLY in the 9q8yy scope
// To query it, you MUST connect to: ws://9q8yy.example.com
```

**Note**: Invalid geohashes are ignored (event stores in current scope). Events are never rejected due to geohash tags.

### Geohash Precision

| Level | Characters | Area | Example | Location |
|-------|------------|------|---------|----------|
| 1 | 1 | ±2500 km | `9` | Western North America |
| 2 | 2 | ±630 km | `9q` | California & Nevada |
| 3 | 3 | ±78 km | `9q8` | SF Bay Area region |
| 4 | 4 | ±20 km | `9q8y` | San Francisco |
| 5 | 5 | ±2.4 km | `9q8yy` | Downtown SF |
| 6 | 6 | ±610 m | `9q8yyk` | Few city blocks |
| 7 | 7 | ±76 m | `9q8yyk2` | Single building |

### Important: Exact Matching & Auto-Routing

Each geohash precision level is completely isolated:

- Event tagged `["g", "9q8yy"]` → stored in `9q8yy` scope → query at `ws://9q8yy.example.com`
- Event tagged `["g", "9q8y"]` → stored in `9q8y` scope → query at `ws://9q8y.example.com`
- Event tagged `["g", "9q8"]` → stored in `9q8` scope → query at `ws://9q8.example.com`

**No hierarchical propagation**: Events at `9q8yy` are NOT visible at `9q8y` or `9q8yyk`

## Configuration

Key environment variables in `.env`:

```bash
DATABASE_PATH=./data
RELAY_PORT=8080
EVENTS_PER_MINUTE=30
ALLOWED_SUBDOMAINS=  # Empty = allow all
```

## Docker

```bash
docker build -t geohashed-relay .
docker run -p 8080:8080 -v ./data:/data geohashed-relay
```

## Building

```bash
cargo build --release
cargo test
```

## License

MIT