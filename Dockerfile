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

# Tell the CDP fetcher where to find the chromium binary. `CdpConfig::default`
# already reads this env var (see crates/fetch/src/cdp.rs).
ENV CHROME_PATH=/usr/bin/chromium

EXPOSE 3002

ENV RUST_LOG=info
ENV HOST=0.0.0.0
ENV PORT=3002

CMD ["crw-server"]