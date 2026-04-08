FROM rust:slim-trixie AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY litebin-common/Cargo.toml litebin-common/Cargo.toml
COPY orchestrator/Cargo.toml orchestrator/Cargo.toml
COPY agent/Cargo.toml agent/Cargo.toml
COPY cli/Cargo.toml cli/Cargo.toml

# Stub sources so cargo can cache deps
RUN mkdir -p litebin-common/src orchestrator/src agent/src cli/src \
    && echo "pub fn placeholder() {}" > litebin-common/src/lib.rs \
    && echo "fn main() {}" > orchestrator/src/main.rs \
    && echo "fn main() {}" > agent/src/main.rs \
    && echo "fn main() {}" > cli/src/main.rs

RUN cargo build --release -p litebin-orchestrator 2>/dev/null || true

# Copy real sources and build
COPY litebin-common/src litebin-common/src
COPY orchestrator/src orchestrator/src

RUN touch litebin-common/src/lib.rs orchestrator/src/main.rs \
    && cargo build --release -p litebin-orchestrator

# Runtime stage
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/litebin-orchestrator /app/litebin-orchestrator

RUN mkdir -p /app/data

EXPOSE 5080

CMD ["/app/litebin-orchestrator"]
