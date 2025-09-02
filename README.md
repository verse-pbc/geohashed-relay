# Geohashed Relay

A Nostr relay that enforces geohash-based routing with complete data isolation per location. Designed for location-based messaging (kind 20000) and geotagged notes (kind 1).

## How It Works

- Events with `["g", "geohash"]` tags MUST be posted to matching subdomain
- Only valid geohash strings allowed as subdomains (prevents arbitrary subdomain creation)
- Each geohash scope is completely isolated - no hierarchical queries

### Common Event Kinds Using Geohash

- **Kind 20000**: Ephemeral location messages (e.g., BitChat proximity chat)
- **Kind 1**: Geotagged text notes
- **Kind 0**: User metadata with location (rare)

### Examples

```javascript
// Location-based message (kind 20000) MUST go to matching subdomain
{
  kind: 20000,
  tags: [["g", "drt2z"]],  // San Francisco geohash
  content: "Anyone nearby for coffee?"
}

// Geotagged note (kind 1)
{
  kind: 1,
  tags: [["g", "drt2z"]],
  content: "Beautiful sunset at Ocean Beach!"
}
// ❌ Rejected at ws://relay.com → "use wss://drt2z.relay.com"
// ✅ Accepted at ws://drt2z.relay.com

// Event without geohash can go anywhere
{
  kind: 1,
  content: "Hello world"
}
// ✅ Accepted at any valid endpoint
```

## Quick Start

```bash
cp .env.example .env
cargo run --release
```

## Configuration

```bash
DATABASE_PATH=./data
RELAY_PORT=8080
EVENTS_PER_MINUTE=60    # Rate limit per connection
```

## Deployment

```bash
docker build -t geohashed-relay .
docker run -p 8080:8080 -v ./data:/data geohashed-relay
```