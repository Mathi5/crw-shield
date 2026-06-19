FROM rust:1.88-bookworm AS builder

WORKDIR /build

# Build tools for BoringSSL (wreq TLS fingerprinting feature)
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    build-essential \
    pkg-config \
    libclang-dev \
    perl \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifest
COPY Cargo.toml Cargo.toml
COPY crates/ crates/

# Build release with TLS fingerprinting enabled.
# `tls-fingerprint` is opt-in (so the lean image stays lean) but we want it
# in production: wreq/BoringSSL gives us byte-perfect Chrome 131/130/128/124,
# Firefox 128/133 and Safari 18 TLS ClientHello + HTTP/2 SETTINGS so we look
# like a real browser to Akamai/DataDome/Cloudflare.
RUN cargo build --release --features crw-fetch/tls-fingerprint

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/crw-server /usr/local/bin/crw-server

EXPOSE 3002

ENV RUST_LOG=info
ENV HOST=0.0.0.0
ENV PORT=3002

CMD ["crw-server"]