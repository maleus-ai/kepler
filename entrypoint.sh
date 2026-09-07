#!/bin/sh
set -e

# In the privileged test container, the cgroup namespace root holds the
# container's own processes, which blocks enabling controllers for its children
# (cgroup v2 "no internal processes" rule) and leaves `memory` undelegated.
# Move those processes into a sub-cgroup so the memory controller can be passed
# down — this reproduces what systemd already does on a real host, where
# /sys/fs/cgroup delegates `memory` out of the box.
if [ "$REQUIRE_CGROUPV2" = "1" ] && [ -w /sys/fs/cgroup/cgroup.subtree_control ]; then
    mkdir -p /sys/fs/cgroup/init
    while read -r pid; do
        echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
    done < /sys/fs/cgroup/cgroup.procs
    echo "+memory" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
    echo "cgroup v2: root subtree_control = [$(cat /sys/fs/cgroup/cgroup.subtree_control)]"
fi

# Build workspace and install binaries to where test harnesses expect them
cargo build --workspace
install target/debug/kepler-exec target/debug/deps/kepler-exec
install target/debug/kepler-daemon target/debug/deps/kepler-daemon
install target/debug/kepler target/debug/deps/kepler

# Run whatever was passed as arguments (default: cargo test --workspace)
exec "$@"
