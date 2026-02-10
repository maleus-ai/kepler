# Testing

How to run tests, the Docker environment, test harnesses, and common patterns.

## Table of Contents

- [Running Tests](#running-tests)
- [Docker Environment](#docker-environment)
- [Test Harnesses](#test-harnesses)
- [Writing Integration Tests](#writing-integration-tests)
- [Writing E2E Tests](#writing-e2e-tests)
- [Common Pitfalls](#common-pitfalls)

---

## Running Tests

Tests require root and a `kepler` group (for socket permissions and privilege dropping). **Always run tests through Docker:**

```bash
# Run all tests (builds image on first run)
docker compose run --rm test

# Run specific test package
docker compose run --rm test cargo test -p kepler-tests

# Run E2E tests
docker compose run --rm test cargo test -p kepler-e2e -- --nocapture

# Run a specific E2E test
docker compose run --rm test cargo test -p kepler-e2e --test dependencies_test -- --nocapture

# Interactive shell inside the container
docker compose run --rm test bash

# Build workspace only
docker compose run --rm test cargo build --workspace

# Clean up containers
docker compose run --rm test
```

---

## Docker Environment

The Docker image provides:
- Root user (required for daemon)
- A `kepler` group with a test user
- All Rust toolchain dependencies
- The full workspace mounted

The `docker compose run --rm test` command handles building the image on first run and mounting the workspace.

---

## Test Harnesses

Kepler provides two test harnesses for different testing needs:

### TestDaemonHarness (kepler-tests)

An **in-process** test harness that starts the daemon within the test process. Located in `kepler-tests/src/helpers/daemon_harness.rs`.

Features:
- Creates temporary state directories
- Starts daemon in-process (no separate binary)
- Provides config builder for programmatic config creation
- Cleans up on drop (kills tracked process groups)
- Marker file utilities for synchronization
- Wait/poll utilities for state assertions

Used by: `kepler-tests` integration tests

### E2eHarness (kepler-e2e)

A **real binary** test harness that spawns actual `kepler` and `kepler-daemon` processes. Located in `kepler-e2e/src/harness.rs`.

Features:
- Builds and runs real binaries
- Tests actual CLI-daemon communication
- Tests real socket permissions and auth
- Provides helper methods: `start_daemon`, `stop_daemon`, `start_services`, `ps`, `logs`, etc.
- Status display assertions (e.g., "Up ", "(healthy)", "Exited", "Failed")

Used by: `kepler-e2e` tests (20 test files)

---

## Writing Integration Tests

Integration tests use `TestDaemonHarness` from `kepler-tests`:

```rust
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::config_builder::ConfigBuilder;

#[tokio::test]
async fn test_service_start() {
    let harness = TestDaemonHarness::new().await;

    let config = ConfigBuilder::new()
        .add_service("web", vec!["python3", "-m", "http.server"])
        .build();

    harness.load_config(config).await;
    harness.start_services().await;

    // Assert service state...
}
```

Key patterns:
- Use `ConfigBuilder` for programmatic config creation
- Use marker files for timing synchronization between test and services
- Use wait utilities for polling service state
- Tests that need env vars at config time should use `ENV_LOCK` + `new_with_env_lock_held`

---

## Writing E2E Tests

E2E tests use `E2eHarness` from `kepler-e2e`:

```rust
use kepler_e2e::harness::E2eHarness;

#[tokio::test]
async fn test_basic_start_stop() {
    let harness = E2eHarness::new().await;
    harness.start_daemon().await;

    // Write config file
    harness.write_config("kepler.yaml", config_content);

    // Start services
    harness.start_services(None).await;

    // Check status
    let ps_output = harness.ps(None).await;
    assert!(ps_output.contains("Up "));

    harness.stop_services(None).await;
    harness.stop_daemon().await;
}
```

Key patterns:
- E2E tests check display text, not internal status enums
- Use `status_to_display_pattern()` to map logical status → display text
- PID extraction: `extract_pid_from_ps()` takes the last token (PID is always last column)
- Status assertions: check "Up " (not "running"), "Exited" (not "stopped" for natural exits)

---

## Common Pitfalls

- **Tests must run in Docker** -- socket permissions and privilege dropping require root + kepler group
- **`TestDaemonHarness::Drop` kills all tracked process groups** -- handles test panics gracefully
- **`build_command()` sets `process_group(0)`** -- spawned processes are their own PGID
- **Process management uses `killpg()`** -- kills entire process groups, not individual PIDs
- **Race conditions** -- use marker files and wait utilities instead of `sleep` for synchronization
- **Env var tests** -- use `ENV_LOCK` to prevent concurrent tests from interfering with each other's env vars

---

## See Also

- [Getting Started](getting-started.md#running-tests) -- Quick test commands
- [Architecture](architecture.md) -- Internal implementation
- [Security Model](security-model.md) -- Security model
