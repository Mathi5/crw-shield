FROM rust:1.88-bookworm AS builder

WORKDIR /build

# Install system deps for chromiumoxide (if needed later)
# For now just Rust build

# Copy workspace manifest
COPY Cargo.toml Cargo.toml
COPY crates/ crates/

# Build release
RUN cargo build --release

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