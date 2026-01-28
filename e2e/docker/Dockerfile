# Dockerfile for running Kepler E2E tests in an isolated environment

FROM rust:1.87-slim-bookworm

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy source code
COPY . .

# Build release binaries
RUN cargo build --release --workspace

ENV RUST_BACKTRACE=1

ENTRYPOINT ["cargo", "test", "-p", "kepler-e2e", "--"]
CMD ["--nocapture"]
