# Multi-stage Dockerfile for cmdh CLI
# Produces minimal runtime images for both amd64 and arm64

# --- Build stage ---
FROM docker.io/library/rust:latest AS builder

WORKDIR /build

# Cache dependencies by building with empty main first
COPY Cargo.toml Cargo.lock ./
COPY cmdhub-shared/Cargo.toml cmdhub-shared/Cargo.toml
COPY cmdhub-cli/Cargo.toml cmdhub-cli/Cargo.toml
COPY cmdhub-mcp/Cargo.toml cmdhub-mcp/Cargo.toml
COPY cmdhub-skills/Cargo.toml cmdhub-skills/Cargo.toml
RUN mkdir -p cmdhub-shared/src cmdhub-cli/src cmdhub-cli/src/bin cmdhub-mcp/src cmdhub-skills/src && \
    echo 'fn main() {}' > cmdhub-cli/src/main.rs && \
    echo 'fn main() {}' > cmdhub-cli/src/bin/extractor.rs && \
    echo 'fn main() {}' > cmdhub-cli/src/bin/build_db.rs && \
    echo '' > cmdhub-cli/src/lib.rs && \
    echo 'fn main() {}' > cmdhub-mcp/src/main.rs && \
    echo '' > cmdhub-shared/src/lib.rs && \
    echo '' > cmdhub-skills/src/lib.rs && \
    cargo build --release && \
    rm -rf cmdhub-shared/src cmdhub-cli/src cmdhub-mcp/src cmdhub-skills/src

# Build the actual binary
COPY . .
RUN touch cmdhub-shared/src/lib.rs cmdhub-cli/src/lib.rs cmdhub-cli/src/main.rs cmdhub-cli/src/bin/extractor.rs cmdhub-cli/src/bin/build_db.rs cmdhub-mcp/src/main.rs cmdhub-skills/src/lib.rs && \
    cargo build --release --locked -p cmdhub-cli

# --- Runtime stage ---
FROM docker.io/library/debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/cmdh /usr/local/bin/cmdh

# Run as non-root user
RUN useradd --create-home appuser
USER appuser

ENTRYPOINT ["cmdh"]
