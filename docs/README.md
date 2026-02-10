# Kepler Documentation

Welcome to the Kepler documentation. This is the central index for all documentation pages.

For a quick overview of the project, see the [main README](../README.md).

---

## Getting Started

- **[Getting Started](getting-started.md)** -- Installation, setup, and your first config
- **[CLI Reference](cli-reference.md)** -- Complete command reference with examples
- **[Configuration](configuration.md)** -- Full YAML schema reference

## Core Concepts

- **[Service Lifecycle](service-lifecycle.md)** -- Status states, start modes, and quiescence
- **[Dependencies](dependencies.md)** -- Conditions, ordering, propagation, and timeouts
- **[Health Checks](health-checks.md)** -- Docker-compatible health check configuration
- **[Hooks](hooks.md)** -- Global and service lifecycle hooks

## Configuration Features

- **[Environment Variables](environment-variables.md)** -- Three-stage expansion, inheritance, and isolation
- **[Variable Expansion](variable-expansion.md)** -- Shell-style `${VAR}` syntax reference
- **[Lua Scripting](lua-scripting.md)** -- Luau sandbox, context, and examples
- **[Log Management](log-management.md)** -- Storage, buffering, retention, and streaming
- **[File Watching](file-watching.md)** -- Auto-restart on file changes

## Security & Operations

- **[Security Model](security-model.md)** -- Root requirement, kepler group, socket auth
- **[Privilege Dropping](privilege-dropping.md)** -- User/group, resource limits

## Internals

- **[Architecture](architecture.md)** -- Internal implementation, design decisions, and diagrams
- **[Protocol](protocol.md)** -- Multiplexed IPC wire format
- **[Testing](testing.md)** -- Docker environment, test harnesses, E2E patterns
