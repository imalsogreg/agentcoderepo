FROM rust:1-bookworm AS builder

WORKDIR /app

# Install build deps for turso/bindgen
RUN apt-get update && apt-get install -y libclang-dev && rm -rf /var/lib/apt/lists/*

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/agentcoderepo-server/Cargo.toml crates/agentcoderepo-server/Cargo.toml
COPY crates/agentcoderepo-git/Cargo.toml crates/agentcoderepo-git/Cargo.toml
COPY crates/agentcoderepo-store/Cargo.toml crates/agentcoderepo-store/Cargo.toml
COPY crates/agentcoderepo-types/Cargo.toml crates/agentcoderepo-types/Cargo.toml
COPY crates/agentcoderepo-llm/Cargo.toml crates/agentcoderepo-llm/Cargo.toml
COPY crates/agentcoderepo-index/Cargo.toml crates/agentcoderepo-index/Cargo.toml
COPY crates/agentcoderepo-test/Cargo.toml crates/agentcoderepo-test/Cargo.toml

# Create dummy source files so cargo can resolve the workspace
RUN for dir in crates/*/; do mkdir -p "$dir/src" && echo "" > "$dir/src/lib.rs"; done && \
    mkdir -p crates/agentcoderepo-server/src && echo "fn main() {}" > crates/agentcoderepo-server/src/main.rs

# Build dependencies only (cached layer)
RUN cargo build --release --bin agentcoderepo-server 2>/dev/null || true

# Copy real source
COPY crates/ crates/

# Touch source files to invalidate the dummy build
RUN find crates -name "*.rs" -exec touch {} +

# Build the actual binary
RUN cargo build --release --bin agentcoderepo-server

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates git && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/agentcoderepo-server /usr/local/bin/

EXPOSE 8080

CMD ["agentcoderepo-server"]
