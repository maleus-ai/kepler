# kepler-e2e

End-to-end test suite that tests real `kepler` and `kepler-daemon` binaries.

## Library + Tests

This crate provides the `E2eHarness` library and 20 test files covering all major features.

## Architecture

Unlike `kepler-tests` (which runs in-process), E2E tests spawn **real binaries** and communicate over the actual Unix socket:

- **E2eHarness** -- Builds binaries, manages daemon lifecycle, provides helpers for running CLI commands and asserting output
- Tests verify actual CLI output, status display text, and process behavior

## Key Modules

| Module | Description |
|--------|-------------|
| `harness.rs` | `E2eHarness`: binary building, daemon start/stop, CLI command helpers, status display assertions |

## Test Files

| Test | Coverage |
|------|----------|
| `attached_start_test` | Foreground mode, Ctrl+C handling |
| `daemon_lifecycle_test` | Daemon start/stop/restart/status |
| `dependencies_test` | All dependency conditions, propagation, timeouts |
| `environment_test` | Env vars, sys_env, env_file |
| `file_watcher_test` | Auto-restart on file changes |
| `healthcheck_test` | Health checks, healthy/unhealthy transitions |
| `hooks_test` | Service lifecycle hooks |
| `integration_test` | General integration scenarios |
| `logs_test` | Log viewing, follow, head/tail |
| `lua_advanced_test` | Advanced Lua scripting |
| `lua_scripting_test` | Basic Lua scripting |
| `multi_config_test` | Multiple config management |
| `prune_start_test` | Config pruning |
| `recreate_test` | Config re-baking |
| `restart_policy_test` | Restart policies |
| `service_lifecycle_test` | Service start/stop/restart |
| `shell_expansion_test` | Variable expansion |
| `signal_test` | Custom stop signals |
| `stop_clean_test` | Stop with cleanup hooks |
| `sys_env_test` | System env inheritance |

## See Also

- [Testing](../docs/testing.md) -- Test guide and patterns
- [CLI Reference](../docs/cli-reference.md) -- Commands being tested
