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
# tls-proxy/ is a separate Go module built in its own stage below.
# We intentionally do NOT copy it into the Rust build context.

# Build release with TLS fingerprinting enabled.
# `tls-fingerprint` is opt-in (so the lean image stays lean) but we want it
# in production: wreq/BoringSSL gives us byte-perfect Chrome 131/130/128/124,
# Firefox 128/133 and Safari 18 TLS ClientHello + HTTP/2 SETTINGS so we look
# like a real browser to Akamai/DataDome/Cloudflare.
#
# `firecrawl-extractor` is also opt-in: enables the 5-stage Firecrawl
# html-extractor pipeline (Apache-2.0) for Article/Doc page types via
# `extract_main_content_v4`. Off by default for the lean build. We enable
# it here because real Article pages (Wikipedia, blogs, news) benefit from
# the upstream pipeline's scoring weights.
RUN cargo build --release \
    --features crw-fetch/tls-fingerprint \
    --features crw-extract/firecrawl-extractor

# ---- Go tls-impersonate-proxy builder ---------------------------------------
# Builds the MITM TLS-impersonation proxy as a sidecar binary. The proxy is
# spawned as a child process by the Rust parent and lets Chrome speak with a
# byte-perfect browser TLS ClientHello (via bogdanfinn/tls-client) instead
# of vanilla BoringSSL — bypasses Cloudflare IUAM and similar.
# See tls-proxy/README.md and references/tls-impersonate-proxy.md.
FROM golang:1.24-bookworm AS tls-proxy-builder

WORKDIR /build/tls-proxy
COPY tls-proxy/go.mod tls-proxy/go.sum ./
RUN go mod download
COPY tls-proxy/ ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
    go build -trimpath -ldflags="-s -w" -o /out/tls-impersonate-proxy .

# Runtime stage — installs the system chromium binary and its dependencies
# so the CDP fetcher can launch a real browser inside the container.
#
# We use Debian's `chromium` package (NOT chrome from Google) — this is
# FOSS, ships with all the libraries the browser needs, and works on
# bookworm without an extra apt source. The CHROME_PATH env var points
# the Rust code at the right binary.
FROM debian:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    chromium \
    chromium-driver \
    fonts-liberation \
    libnss3 \
    libatk-bridge2.0-0 \
    libgtk-3-0 \
    libgbm1 \
    libasound2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/crw-server /usr/local/bin/crw-server

# tls-impersonate-proxy sidecar binary (built in the tls-proxy-builder stage above).
# Spawned as a child process by the Rust parent when TLS_PROXY_ENABLED=true.
# Default location; can be overridden via TLS_PROXY_BINARY env var.
COPY --from=tls-proxy-builder /out/tls-impersonate-proxy /usr/local/bin/tls-impersonate-proxy
RUN chmod 755 /usr/local/bin/tls-impersonate-proxy

# Tell the CDP fetcher where to find the chromium binary. `CdpConfig::default`
# already reads this env var (see crates/fetch/src/cdp.rs).
ENV CHROME_PATH=/usr/bin/chromium

EXPOSE 3002

ENV RUST_LOG=info
ENV HOST=0.0.0.0
ENV PORT=3002

CMD ["crw-server"]