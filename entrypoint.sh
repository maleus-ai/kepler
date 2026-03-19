#!/bin/sh
set -e

# Build workspace and install binaries to where test harnesses expect them
cargo build --workspace
install target/debug/kepler-exec target/debug/deps/kepler-exec
install target/debug/kepler-daemon target/debug/deps/kepler-daemon
install target/debug/kepler target/debug/deps/kepler

# Run whatever was passed as arguments (default: cargo test --workspace)
exec "$@"
