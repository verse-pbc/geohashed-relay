#!/bin/bash

# Copy the .env.example if .env doesn't exist
if [ ! -f .env ]; then
    cp .env.example .env
    echo "Created .env from .env.example - please customize as needed"
fi

# Create data directory if it doesn't exist
mkdir -p ./data

# Run the relay
echo "Starting Scoped Relay..."
cargo run --release