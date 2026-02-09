FROM rust:1.93-slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev sudo g++ linux-perf && rm -rf /var/lib/apt/lists/*

# Install clippy and cargo-flamegraph (inferno-based)
RUN rustup component add clippy \
    && cargo install inferno

# Create kepler group and test users
RUN groupadd kepler \
    && useradd -m -s /bin/bash testuser1 \
    && useradd -m -s /bin/bash testuser2 \
    && useradd -m -s /bin/bash noaccess \
    && usermod -aG kepler testuser1 \
    && usermod -aG kepler testuser2

# Create kepler state directory with correct ownership
RUN mkdir -p /var/lib/kepler \
    && chown root:kepler /var/lib/kepler \
    && chmod 0770 /var/lib/kepler

WORKDIR /app

ENV RUST_BACKTRACE=1

CMD ["cargo", "build", "--workspace"]
