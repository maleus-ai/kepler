# Getting Started

This guide walks you through installing Kepler, setting up your system, and running your first services.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Installing from Source](#installing-from-source)
- [Install Options](#install-options)
- [Post-Install Setup](#post-install-setup)
- [Quick Start Tutorial](#quick-start-tutorial)
- [Systemd Integration](#systemd-integration)
- [Verifying Installation](#verifying-installation)
- [Running Tests](#running-tests)

---

## Prerequisites

- **Linux** (Unix socket and privilege-dropping features require Linux)
- **Rust toolchain** (1.85+): Install via [rustup](https://rustup.rs/)
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

---

## Installing from Source

```bash
git clone https://github.com/your-org/kepler.git
cd kepler
./install.sh
```

The install script performs the following steps:

1. Builds release binaries (`cargo build --release`)
2. Installs binaries to `<prefix>/bin/`
3. Creates the `kepler` group (if it doesn't exist)
4. Creates `/var/lib/kepler/` with `root:kepler` ownership and `0770` permissions
5. Optionally installs a systemd service file and enables it

### Installed Binaries

| Binary | Description |
|--------|-------------|
| `kepler` | CLI client (used by all users in the `kepler` group) |
| `kepler-daemon` | Daemon process (must run as root) |
| `kepler-exec` | Privilege-dropping wrapper (used internally by the daemon) |

---

## Install Options

```bash
./install.sh --no-systemd   # Skip systemd service installation
./install.sh --no-build     # Skip build, use existing target/release binaries
./install.sh --uninstall    # Remove binaries and systemd service
```

---

## Post-Install Setup

The install script will offer to add your user to the `kepler` group. **You must log out and log back in** for group changes to take effect.

To add other users later:

```bash
sudo usermod -aG kepler otheruser  # then log out/in
```

Verify group membership:

```bash
groups  # should include "kepler"
```

---

## Quick Start Tutorial

### 1. Create a configuration file

Create a `kepler.yaml` in your project directory:

```yaml
services:
  backend:
    command: ["npm", "run", "start"]
    working_dir: ./backend
    restart: always
    healthcheck:
      test: ["sh", "-c", "curl -f http://localhost:3000/health || exit 1"]
      interval: 10s
      timeout: 5s
      retries: 3
```

### 2. Start the daemon and services

```bash
# Start daemon in background (requires root)
kepler daemon start -d

# Start services, follow logs (Ctrl+C to stop)
kepler start

# Or start detached
kepler start -d

# Or start detached and wait for startup cluster
kepler start -d --wait
```

### 3. Monitor and manage

```bash
kepler ps                # Show service status
kepler logs --follow     # Follow logs
kepler stop              # Stop services (SIGTERM)
kepler stop -s SIGKILL   # Stop services with a specific signal
kepler daemon stop       # Stop daemon
```

For a ready-to-run example, see [`example.kepler.yaml`](../example.kepler.yaml) in the project root.

---

## Systemd Integration

If installed with systemd support (the default), you can manage the daemon via systemd:

```bash
sudo systemctl start kepler     # Start daemon
sudo systemctl stop kepler      # Stop daemon
sudo systemctl restart kepler   # Restart daemon
sudo systemctl status kepler    # Check daemon status
sudo systemctl enable kepler    # Enable on boot
```

Or start the daemon manually:

```bash
sudo kepler daemon start -d
```

---

## Verifying Installation

```bash
kepler --version

# Start daemon (requires root)
sudo kepler daemon start -d
kepler daemon status
kepler daemon stop
```

---

## Running Tests

Tests require root and a `kepler` group (for socket permissions and privilege dropping). Always run tests through Docker:

```bash
# Run all tests (builds image on first run)
docker compose run --rm test

# Run specific test package
docker compose run --rm test cargo test -p kepler-tests

# Run E2E tests
docker compose run --rm test cargo test -p kepler-e2e -- --nocapture

# Interactive shell inside the container
docker compose run --rm test bash

# Build workspace only
docker compose run --rm test cargo build --workspace
```

See [Testing](testing.md) for more details on the test harnesses and patterns.

---

## See Also

- [CLI Reference](cli-reference.md) -- Full command reference
- [Configuration](configuration.md) -- YAML schema reference
- [Service Lifecycle](service-lifecycle.md) -- How services start, stop, and restart
