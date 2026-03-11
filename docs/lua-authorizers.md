# Lua Authorizers

Complete reference for Kepler's Lua authorizer system — function signature, available fields, sandbox, and shared code.

## Table of Contents

- [Overview](#overview)
- [Where Authorizers Can Be Defined](#where-authorizers-can-be-defined)
- [Execution Order](#execution-order)
- [Function Signature](#function-signature)
  - [The `request` Table](#the-request-table)
  - [The `caller` Table](#the-caller-table)
- [Request Params Reference](#request-params-reference)
- [Sandbox](#sandbox)
- [Shared Lua Code](#shared-lua-code)
- [Aliases](#aliases)
- [Key Properties](#key-properties)

---

## Overview

Authorizers are Lua functions that run **after** the static rights check passes. They act as rejection filters — all matching authorizers must return `true` for the request to proceed. Authorizers can only deny; they cannot grant access beyond what `allow` already provides.

---

## Where Authorizers Can Be Defined

| Location | Scope | When compiled |
|----------|-------|---------------|
| `kepler.acl.users.<name>.authorize` | Per-user ACL rule | Config load / recreate |
| `kepler.acl.groups.<name>.authorize` | Per-group ACL rule | Config load / recreate |
| `permissions.authorize` | Per-service token | Service start |

---

## Execution Order

When a request passes the static rights check, authorizers are evaluated in order:

1. **Token authorizer** (if the caller is token-authenticated and `permissions.authorize` is set)
2. **User ACL authorizer** (if the caller's UID matches a user rule with `authorize`)
3. **Group ACL authorizers** (all matching group rules with `authorize`, sorted by GID)

All authorizers must return a truthy value. If any returns `false`, `nil`, or raises an error, the request is denied.

---

## Function Signature

Authorizer source is wrapped as `function(request, caller) ... end`. The two arguments are read-only tables.

### The `request` Table

| Field | Type | Description |
|-------|------|-------------|
| `action` | `string` | The action being performed (see [Request Params Reference](#request-params-reference)) |
| `params` | `table` | Action-specific parameters (see [Request Params Reference](#request-params-reference)) |

### The `caller` Table

| Field | Type | Description |
|-------|------|-------------|
| `uid` | `number` | Caller's user ID |
| `gid` | `number` | Caller's primary group ID |
| `username` | `string?` | Caller's username (`nil` if not resolvable) |
| `groups` | `number[]` | All caller GIDs (primary + supplementary) |
| `token` | `boolean` | Whether this is a token-based request |

---

## Request Params Reference

Each action populates `request.params` with different fields. Optional fields (`?` suffix) are `nil` when not provided by the caller.

### `start`

Start service(s) for a config.

| Param | Type | Description |
|-------|------|-------------|
| `services` | `string[]` | Services to start (empty table = all) |
| `no_deps` | `boolean` | Skip dependency waiting and `if:` conditions |
| `hardening` | `string?` | Hardening level (`"none"`, `"no-root"`, `"strict"`) |
| `override_envs` | `table<string, string>?` | Environment variable overrides |

### `stop`

Stop service(s).

| Param | Type | Description |
|-------|------|-------------|
| `services` | `string[]` | Services to stop (empty table = all) |
| `clean` | `boolean` | Whether to cleanup everything after stopping |
| `signal` | `string?` | Signal to send (e.g. `"SIGKILL"`, `"TERM"`, `"9"`) |

### `restart`

Restart service(s).

| Param | Type | Description |
|-------|------|-------------|
| `services` | `string[]` | Services to restart (empty table = all running) |
| `no_deps` | `boolean` | Skip dependency ordering |
| `override_envs` | `table<string, string>?` | Environment variable overrides |

### `recreate`

Stop, re-bake config snapshot, start.

| Param | Type | Description |
|-------|------|-------------|
| `hardening` | `string?` | Hardening level (`"none"`, `"no-root"`, `"strict"`) |

### `status`

Get service status for a specific config. No params.

### `inspect`

Inspect config and runtime state. No params.

### `logs`

Retrieve or subscribe to logs. Maps to both `LogsStream` and `SubscribeLogs` protocol requests.

| Param | Type | Description |
|-------|------|-------------|
| `services` | `string[]` | Services to query (empty table = all). Only set for `LogsStream` |
| `filter` | `string?` | SQL filter expression. Only set for `LogsStream` with filter |

### `subscribe`

Subscribe to service state change events.

| Param | Type | Description |
|-------|------|-------------|
| `services` | `string[]` | Services to watch (empty table = all) |

### `quiescence`

Check if all services are quiescent (settled). No params.

### `readiness`

Check if all services are ready (reached target state). No params.

---

## Sandbox

The ACL Lua VM is stricter than the config Lua VM:

- No `require`, `load`, `loadstring`, `dofile`
- No `io` or `debug` libraries
- `os` limited to `os.clock()` and `os.time()`
- No `global` table
- Standard library tables are frozen (e.g. `string.custom = true` raises an error)
- `json.stringify()` and `json.parse()` are available
- Timeout: warning at 100ms, hard cutoff at 1s

---

## Shared Lua Code

The `kepler.acl.lua` field defines shared functions available to all authorizers in the same ACL. This code is evaluated once when the ACL is loaded, before any authorizer is compiled:

```yaml
kepler:
  acl:
    lua: |
      allowed_services = { web = true, api = true }
      function can_stop(service)
        return allowed_services[service] == true
      end
    users:
      alice:
        allow: [start, stop, restart, status]
        authorize: |
          if request.action == "stop" then
            for _, svc in ipairs(request.params.services) do
              if not can_stop(svc) then return false end
            end
          end
          return true
```

---

## Aliases

User-defined aliases group rights under a name for reuse:

```yaml
kepler:
  acl:
    aliases:
      viewer: [status, logs]
      operator: [start, stop, restart, status, logs]
    users:
      alice:
        allow: [operator]       # Expands to [start, stop, restart, status, logs]
      bob:
        allow: [viewer]         # Expands to [status, logs]
```

Alias rules:
- Expanded at config parse time (not at request time)
- Cannot shadow built-in aliases (`all`) or known right names
- Max expansion depth: 2 levels (aliases can reference other aliases)

---

## Key Properties

- **Authorizers can only deny** — returning `true` does not grant rights beyond `allow`
- **Static rights are checked first** — if the caller lacks the right in `allow`, the authorizer never runs
- **Root bypasses authorizers** — root is never subject to authorizer checks
- **Token authorizers run in the same VM** as ACL authorizers but are compiled separately at service start

---

## See Also

- [Security Model](security-model.md) — ACL, rights, effective permissions, full authorization pipeline
- [Configuration](configuration.md) — `acl` and `permissions` fields in YAML
- [Lua Scripting](lua-scripting.md) — Config Lua VM (different from ACL VM)
