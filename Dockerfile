# ─── Stage 1: Builder ─────────────────────────────────────────────────────────
FROM rust:1.78-slim AS builder

# Install build deps
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependencies: copy manifests first, build deps only
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf src

# Copy source & build
COPY src ./src
COPY static ./static
# Touch main.rs to force rebuild
RUN touch src/main.rs && cargo build --release

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Non-root user
RUN useradd -m -u 1000 proxy
USER proxy

WORKDIR /app

# Copy binary + static assets
COPY --from=builder /build/target/release/claude-codex-proxy ./
COPY --from=builder /build/static ./static

# Volumes:
#   /data        → SQLite database file
#   /config      → accounts.toml + auth JSON files
VOLUME ["/data", "/config"]

# Default env
ENV PROXY_PORT=8080 \
    RUST_LOG=info \
    DB_PATH=/data/proxy.db \
    ACCOUNTS_CONFIG_PATH=/config/accounts.toml \
    PROXY_AUTH_PATH=/config/auth.json

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD wget -qO- http://localhost:8080/health || exit 1

ENTRYPOINT ["./claude-codex-proxy"]
CMD ["serve"]
