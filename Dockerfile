# Multi-stage build for the drm3-rpc-pool JSON-RPC failover proxy.
#
#   docker build -t drm3-rpc-pool .
#   docker run --rm -p 8545:8545 \
#     -e ALCHEMY_KEY=... \
#     -v "$PWD/rpc-pool.toml:/etc/drm3/rpc-pool.toml:ro" \
#     drm3-rpc-pool
#
# Remember to set `listen = "0.0.0.0:8545"` in your config so the proxy is
# reachable from outside the container.

# ── Builder ───────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

WORKDIR /build

# Cache dependencies: copy manifests first, build a stub, then the real source.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --bin drm3-rpc-pool 2>/dev/null || true
RUN rm -rf src

COPY src ./src
# Touch so cargo rebuilds with the real sources.
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --bin drm3-rpc-pool

# ── Runtime ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# rustls is used (no OpenSSL); only CA certs are needed for outbound TLS.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --system --uid 10001 --no-create-home drm3
USER drm3

COPY --from=builder /build/target/release/drm3-rpc-pool /usr/local/bin/drm3-rpc-pool

EXPOSE 8545

# Mount your config at this path (or override the --config flag).
ENTRYPOINT ["drm3-rpc-pool"]
CMD ["--config", "/etc/drm3/rpc-pool.toml"]
