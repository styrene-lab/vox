# Multi-stage build for vox OCI image.
#
# Feature selection at build time controls which connectors are linked:
#   podman build --build-arg FEATURES=discord -t vox .
#   podman build --build-arg FEATURES=all -t vox-full .
#
# Runtime modes (set via command override):
#   vox --rpc                          # omegon extension over stdio (default)
#   vox --bridge --daemon-url URL      # push events to omegon daemon
#
# For airgapped deployments: build once, `podman save`, transfer,
# `podman load` on target. No network access needed at runtime.

ARG FEATURES="all"

# --- Build stage ---
FROM rust:1-bookworm AS builder

ARG FEATURES

WORKDIR /build

# Copy workspace manifests first for dependency caching
COPY Cargo.toml ./
COPY vox-core/Cargo.toml vox-core/
COPY vox/Cargo.toml vox/
COPY vox-signal/Cargo.toml vox-signal/
COPY vox-email/Cargo.toml vox-email/
COPY vox-lxmf/Cargo.toml vox-lxmf/
COPY vox-voice/Cargo.toml vox-voice/
COPY vox-slack/Cargo.toml vox-slack/
COPY vox-discord/Cargo.toml vox-discord/

# Create stub lib files for dependency caching layer
RUN mkdir -p vox-core/src vox/src vox-signal/src vox-email/src vox-lxmf/src \
             vox-voice/src vox-slack/src vox-discord/src \
    && echo "fn main() {}" > vox/src/main.rs \
    && touch vox-core/src/lib.rs vox-signal/src/lib.rs vox-email/src/lib.rs \
             vox-lxmf/src/lib.rs vox-voice/src/lib.rs vox-slack/src/lib.rs \
             vox-discord/src/lib.rs

# Pre-build dependencies (cached unless Cargo.toml changes)
RUN cargo build --release -p vox --features "${FEATURES}" 2>/dev/null || true

# Copy real source
COPY . .

# Touch source files to invalidate the stub cache
RUN touch vox-core/src/lib.rs vox/src/main.rs \
          vox-signal/src/lib.rs vox-email/src/lib.rs \
          vox-lxmf/src/lib.rs vox-voice/src/lib.rs \
          vox-slack/src/lib.rs vox-discord/src/lib.rs

# Build for real
RUN cargo build --release -p vox --features "${FEATURES}"
RUN strip target/release/vox

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/vox /usr/local/bin/vox

# Default config location — mount or bake your vox.toml here
RUN mkdir -p /etc/vox
COPY config.example.toml /etc/vox/vox.toml

ENV VOX_CONFIG=/etc/vox/vox.toml

# Default: extension mode (JSON-RPC over stdio, managed by omegon).
# Override for bridge mode:
#   CMD ["vox", "--bridge", "--daemon-url", "http://omegon:7842"]
ENTRYPOINT ["vox", "--rpc"]
