# Build stage
FROM rust:1.87-slim AS builder

WORKDIR /build

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev protobuf-compiler && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binaries
RUN cargo build --workspace --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy binaries
COPY --from=builder /build/target/release/varaha-cache /usr/local/bin/varaha-cache
COPY --from=builder /build/target/release/rv-admin-cli /usr/local/bin/rv-admin-cli
COPY --from=builder /build/target/release/rv-log-viewer /usr/local/bin/rv-log-viewer
COPY --from=builder /build/target/release/rv-stat /usr/local/bin/rv-stat

# Copy example configs
COPY examples/ /etc/varaha-cache/examples/

EXPOSE 6081 6082

ENTRYPOINT ["varaha-cache"]
