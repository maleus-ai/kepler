# kepler-tests

Integration test library providing the `TestDaemonHarness` and test utilities.

## Library

This is a library crate providing test helpers. Integration tests live in the `tests/` directory and use this library.

## Architecture

The test harness runs the daemon **in-process** for fast, isolated integration testing:

- **TestDaemonHarness** -- Creates temporary state directories, starts the daemon in-process, and provides API for loading configs and managing services
- **ConfigBuilder** -- Programmatic config construction for tests
- **Marker files** -- File-based synchronization between test code and service processes
- **Wait utilities** -- Polling helpers for state assertions

## Key Modules

| Module | Description |
|--------|-------------|
| `helpers/daemon_harness.rs` | `TestDaemonHarness`: temporary dirs, in-process daemon, process group tracking, cleanup on drop |
| `helpers/config_builder.rs` | `ConfigBuilder`: fluent API for building test configs |
| `helpers/marker_files.rs` | File-based synchronization utilities |
| `helpers/wait_utils.rs` | Polling and timeout helpers for async assertions |

## See Also

- [Testing](../docs/testing.md) -- Test guide and patterns
- [Architecture](../docs/architecture.md) -- Internal implementation
