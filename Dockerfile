# Stage 1: Build the application
FROM rust:1.85.1 as builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests and lock file
COPY Cargo.toml Cargo.lock ./

# Create dummy src files to cache dependencies
RUN mkdir src && \
    echo "fn main(){}" > src/main.rs && \
    echo "// placeholder" > src/lib.rs

# Build dependencies (this layer is cached if manifests don't change)
RUN cargo build --release --bin geohashed-relay

# Copy the actual source code
COPY . .

# Build the application binary, leveraging cached dependencies
RUN touch src/main.rs && cargo build --release --bin geohashed-relay

# Stage 2: Create the final lean image
FROM debian:bookworm-slim

# Install runtime dependencies (use libssl3 for bookworm)
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

# Create app user for security
RUN useradd -ms /bin/bash -u 1001 appuser

# Set working directory
WORKDIR /app

# Create data directory with proper permissions
RUN mkdir -p /data && chown appuser:appuser /data

# Copy the compiled binary from the builder stage
COPY --from=builder /app/target/release/geohashed-relay .

# Change ownership of the binary
RUN chown appuser:appuser ./geohashed-relay

# Switch to non-root user
USER appuser

# Expose the WebSocket port
EXPOSE 8080

# Health check for WebSocket endpoint
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Set default environment variables
ENV RUST_LOG=info
ENV DATABASE_PATH=/data
ENV RELAY_HOST=0.0.0.0
ENV RELAY_PORT=8080

# Define the entrypoint
ENTRYPOINT ["./geohashed-relay"]