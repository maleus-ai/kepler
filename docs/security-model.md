# Security Model

Kepler's security design: root requirement, group-based access, per-config ACLs with Lua authorizers, token-based service permissions (via the `permissions` field), environment isolation, sandboxing, and hardening.

## Table of Contents

- [Root Requirement](#root-requirement)
- [The kepler Group](#the-kepler-group)
- [Socket Security](#socket-security)
- [State Directory Security](#state-directory-security)
- [Per-Config ACL](#per-config-acl)
  - [Lua Authorizers](#lua-authorizers)
- [Filesystem Read Access](#filesystem-read-access)
- [Service Permissions (Token-Based)](#service-permissions-token-based)
- [Effective Permissions](#effective-permissions)
  - [Full Authorization Pipeline](#full-authorization-pipeline)
- [Hardening](#hardening)
- [Environment Isolation](#environment-isolation)
- [Lua Sandbox](#lua-sandbox)
- [Log Query Security](#log-query-security)
- [Privilege Dropping](#privilege-dropping)

---

## Root Requirement

The daemon must run as root. This is enforced unconditionally on startup -- the daemon exits with an error if not running as root.

Root is required to:
- **Drop privileges** per-service (`setuid`/`setgid`/`initgroups` to configured `user`/`groups`)
- **Create and chown** the state directory and socket to `root:kepler`
- **Set resource limits** on spawned processes (`setrlimit`)

```bash
sudo kepler daemon start -d    # Must be root
```

---

## The `kepler` Group

CLI access to the daemon is controlled via the `kepler` group:

- The install script creates the `kepler` group if it doesn't exist
- Users must be members of the `kepler` group to communicate with the daemon
- Root users always have access regardless of group membership
- By default, only root and the config owner can operate on a config
- Config owners can grant access to other group members via a [per-config ACL](#per-config-acl)

### Adding Users

```bash
sudo usermod -aG kepler username    # Add to `kepler` group
# User must log out and back in for changes to take effect
```

### Verifying Membership

```bash
groups    # Should include "kepler"
```

---

## Socket Security

The daemon creates a Unix domain socket with strict permissions:

- **Path**: `/var/lib/kepler/kepler.sock` (or `$KEPLER_SOCKET_PATH` if set)
- **Permissions**: `0o666` (`rw-rw-rw-`)
- **Ownership**: `root:kepler`

### Peer Credential Verification

Every connection is verified using Unix peer credentials. Root is always allowed. Other users must be in the `kepler` group (checked via both primary and supplementary group membership). Clients not in the `kepler` group are rejected.

See [Architecture](architecture.md#peer-credential-verification) for implementation details.

---

## State Directory Security

The daemon state directory is secured:

- **Path**: `/var/lib/kepler/` (or `$KEPLER_DAEMON_PATH`)
- **Permissions**: `0o771` (`rwxrwx--x`) -- **enforced at every startup**
- **Ownership**: `root:kepler`
- **Daemon umask**: `0o007` on startup

### Startup Hardening

The daemon validates and secures the state directory at every startup. Symlink attacks are rejected, permissions are enforced to `0o771`, and world-readable/writable bits are verified absent.

See [Architecture](architecture.md#state-directory-hardening) for implementation details.

### Contents

- `kepler.sock` -- Unix domain socket (`0o666`), or at `$KEPLER_SOCKET_PATH` if set
- `kepler.pid` -- Daemon PID file (`0o660`)
- `configs/` -- Per-config state directories

---

## Per-Config ACL

By default, only root and the config owner have access to a config. The config owner is the user who loaded the config (via `kepler start` or `kepler recreate` -- recreating a config transfers ownership to the caller). The `kepler.acl` section allows config owners to grant specific permissions to other `kepler` group members.

### Access Rules

Rights-gated operations are requests that require specific rights (e.g., `start`, `status`). Some operations like `kepler ps --all` and `kepler prune` are not governed by per-config ACL rights — they are always allowed through the ACL gate but may still be restricted by other checks (e.g., `kepler daemon stop` and `kepler prune` are root-only).

1. **Root (UID 0)**: Always has full access -- ACLs are never checked for root
2. **Config owner** (the user who first loaded the config): Always has full access
3. **Other `kepler` group members**:
   - No `acl` section in config → all rights-gated operations are denied
   - `acl` section present → access restricted to matching user/group rules
   - No matching rules → all rights-gated operations are denied
   - An empty `acl: {}` section behaves the same as no `acl` section -- non-owner group members are denied all rights-gated operations because no rules match
4. **Token-based auth**: `effective = Process.allow ∩ ACL(uid, gid)`. Root (uid 0) and config owner have implicit `ACL = *`. See [Effective Permissions](#effective-permissions)

### Configuration

ACLs are defined under `kepler.acl` with `users` and `groups` sub-sections. Keys can be usernames, group names, or numeric UIDs/GIDs. Each rule has an `allow` list and an optional `authorize` Lua authorizer.

```yaml
kepler:
  acl:
    lua: |
      -- Shared functions available to all authorizers (optional)
      function is_business_hours()
        local hour = os.time() % 86400 / 3600
        return hour >= 9 and hour < 17
      end
    aliases:                    # User-defined right bundles (optional)
      viewer: [status, logs]
      operator: [start, stop, restart, status, logs]
    users:
      alice:                    # Resolved to UID at config load time
        allow: [operator]       # Expands alias → [start, stop, restart, status, logs]
      1001:                     # Numeric UID
        allow: [start, stop, status]
        authorize: |            # Optional Lua authorizer (can only deny)
          if request.action == "stop" and not is_business_hours() then
            return false
          end
          return true
    groups:
      developers:               # Resolved to GID at config load time
        allow: [viewer]         # Expands alias → [status, logs]
      2000:                     # Numeric GID
        allow: [status]
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `lua` | `string` | No | Inline Lua code evaluated in the ACL Lua VM. Defines shared functions for authorizers. Separate from the config's main Lua VM. |
| `aliases` | `object` | No | Named bundles of rights, expanded at parse time. Cannot shadow built-in aliases (`all`) or known rights. Max expansion depth: 2 levels. |
| `users` | `object` | No | User rules keyed by username or numeric UID. |
| `groups` | `object` | No | Group rules keyed by group name or numeric GID. |

Each rule accepts:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `allow` | `string[]` | Yes | Rights granted to this user/group. Supports aliases. |
| `authorize` | `string` | No | Lua authorizer source. Called after static rights check. Must return truthy to allow, false/nil to deny. See [Lua Authorizers](#lua-authorizers). |

### Rights

Rights are flat, explicit identifiers — there are no categories, no implicit cascading, and no hidden expansions.

| Type | Description |
| ---- | ----------- |
| Base right | Gates exactly one request type (e.g., `start`, `stop`, `status`) |
| Sub-right | Gates an optional feature within a request. Format: `base:feature` (e.g., `stop:clean`, `start:env-override`). Meaningless without its base right. |
| Alias | Named bundle expanded at config parse time. Only `all` is built-in. User-defined aliases go in `kepler.acl.aliases`. |

### Available Rights

**Base rights:**

| Right        | Grants                                                                |
| ------------ | --------------------------------------------------------------------- |
| `start`      | Start services                                                        |
| `stop`       | Stop services                                                         |
| `restart`    | Restart services                                                      |
| `recreate`   | Recreate config                                                       |
| `status`     | View config/service status; filters `kepler ps --all` results |
| `inspect`    | Inspect config                                                        |
| `logs`       | View service logs and subscribe to log streams                        |
| `subscribe`  | Subscribe to service state changes                                    |
| `quiescence` | Check if system reached quiescence                                    |
| `readiness`  | Check if services are ready                                           |

**Sub-rights:**

| Sub-right              | Base      | Grants                                                                |
| ---------------------- | --------- | --------------------------------------------------------------------- |
| `start:env-override`   | `start`   | Allow environment variable overrides (`-e`) on start                  |
| `start:hardening`      | `start`   | Allow `--hardening` flag on start                                     |
| `start:no-deps`        | `start`   | Allow `--no-deps` flag on start                                       |
| `stop:clean`           | `stop`    | Allow `--clean` flag on stop                                          |
| `stop:signal`          | `stop`    | Allow custom signal on stop                                           |
| `restart:env-override` | `restart` | Allow environment variable overrides on restart                       |
| `restart:no-deps`      | `restart` | Allow `--no-deps` flag on restart                                     |
| `recreate:hardening`   | `recreate`| Allow `--hardening` flag on recreate                                  |
| `logs:search`          | `logs`    | Allow log filter expressions (DSL). See [Log Query Security](#log-query-security) |
| `logs:search:sql`      | `logs`    | Allow raw SQL filter expressions. Requires `logs:search`. See [Log Query Security](#log-query-security) |

**Built-in alias:**

| Alias | Expands to |
| ----- | ---------- |
| `all` | Every base right + every sub-right |

> **Note:** Rights have no implications. `start` does NOT grant `stop`. A sub-right like `stop:clean` requires `stop` to also be granted — it is meaningless on its own.

### Rule Matching

When a request arrives from a non-owner group member:

1. The daemon looks up the caller's UID in `acl.users`
2. The daemon looks up the caller's primary GID and all supplementary GIDs in `acl.groups`
3. All matching rights are **unioned** (a user matching both a user rule and a group rule gets the combined rights)
4. The request's required rights are checked against the union
5. If no rules match at all, all rights-gated operations are denied

### Global Query Filtering

For requests that return cross-config results (`kepler ps --all`), the daemon filters results by read access. Non-owner group members only see configs where they have the `status` right. Configs without an `acl` section are only visible to root and the config owner.

### Daemon-Level Operations

Daemon-level operations (`kepler daemon stop`, `kepler prune`) are restricted to root (UID 0) only. Non-root `kepler` group members cannot shut down the daemon or prune configs. There are no corresponding rights for these operations -- they are enforced exclusively via UID check.

### Cross-Config Limitation

Process permissions are scoped to the config that registered them. A process authenticated by config A cannot operate on config B's services, unless config B was loaded by that process. Child process permissions are bounded by a [permission ceiling](#permission-ceiling-token-inheritance): `child.allow = expand(permissions.allow) ∩ caller_ceiling`.

### Resolution

User and group names are resolved to numeric UIDs/GIDs **at config load time** using the system user/group databases. This means:

- Name changes after config load have no effect until `kepler recreate`
- Numeric UIDs/GIDs are always accepted and don't require system lookup
- Invalid names cause a config load error

### Examples

#### Read-only access for a team

```yaml
kepler:
  acl:
    groups:
      ops-team:
        allow: [status, logs]
```

Only the config owner can start/stop services. Members of `ops-team` can view status and logs. Other `kepler` group members are denied all rights-gated operations.

#### Operator and viewer roles

```yaml
kepler:
  acl:
    users:
      alice:
        allow: [start, stop, restart, status, logs, logs:search]  # Service control + status + logs with DSL search
      bob:
        allow: [status]                                            # Can only view status
    groups:
      developers:
        allow: [status, logs]                                      # Can view status and stream logs (no search)
```

Alice gets full service control and log access (including search). Bob can only check status. All developers can read status and stream logs. The config owner has unrestricted access regardless.

#### Grant full access to all group members

To allow all `kepler` group members unrestricted access (similar to a shared config), use the `all` alias:

```yaml
kepler:
  acl:
    groups:
      kepler:
        allow: [all]           # All `kepler` group members get full access

services:
  app:
    command: ["./app"]
```

Without an `acl` section, only root and the config owner can operate on the config.

### Lua Authorizers

Authorizers are Lua functions that run **after** the static rights check passes. They act as rejection filters — all matching authorizers must return `true` for the request to proceed. Authorizers can only deny; they cannot grant access beyond what `allow` already provides.

Authorizers can be defined on ACL user/group rules (`authorize` field) and on service token permissions (`permissions.authorize`). When a request passes the static rights check, authorizers are evaluated in order:

1. **Token authorizer** (if the caller is token-authenticated and `permissions.authorize` is set)
2. **User ACL authorizer** (if the caller's UID matches a user rule with `authorize`)
3. **Group ACL authorizers** (all matching group rules with `authorize`, sorted by GID)

Each authorizer receives two read-only tables — `request` (with `action` and `params`) and `caller` (with `uid`, `gid`, `username`, `groups`, `token`):

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

Key properties:
- **Authorizers can only deny** — returning `true` does not grant rights beyond `allow`
- **Static rights are checked first** — if the caller lacks the right, the authorizer never runs
- **Root bypasses authorizers** — root is never subject to authorizer checks

See [Lua Authorizers](lua-authorizers.md) for the full reference: per-action `request.params` fields, sandbox constraints, shared code, and aliases.

---

## Filesystem Read Access

Non-root `kepler` group members must have Unix read permission on config files they reference. This check is complementary to the [Per-Config ACL](#per-config-acl) -- both must pass for a request to proceed.

### How It Works

When a non-root user sends a request that references a config path, the daemon checks the file's Unix permission bits:

1. **Owner match** (caller UID == file UID): requires owner read bit (`0o400`)
2. **Group match** (caller's primary or supplementary GIDs include file GID): requires group read bit (`0o040`)
3. **Other**: requires other read bit (`0o004`)

If none of these checks pass, the request is denied with a "Permission denied" error.

### Bypasses

- **Root callers** (UID 0): always bypass this check
- **Token-based callers**: also subject to this check (peer credentials carry UID/GID)

### Relationship to ACL

Both checks are enforced independently:

- A group member with ACL access to a config is still denied if they can't read the config file on disk
- A group member with read access to the file is still denied if they have no matching ACL entry

This prevents users from operating on config files they couldn't read directly, even if they know the path.

---

## Service Permissions (Token-Based)

The `permissions` field on a service controls what daemon operations a spawned service process can perform. When present, the daemon generates a secure bearer token, registers it with the granted rights, and passes it to the spawned process via the `KEPLER_TOKEN` environment variable. The process presents this token in each request, and the daemon looks up its permissions. See [Effective Permissions](#effective-permissions) for how rights interact with ACLs.

> **Backward compatibility:** The field `security` is accepted as an alias for `permissions`.

### How It Works

1. Config owner defines `permissions` on a service with allowed rights
2. At service start, the daemon generates a secure bearer token and registers it with the granted rights
3. The daemon sets `KEPLER_TOKEN` (hex-encoded, 64 chars) in the service's environment and spawns the process
4. The spawned process (and any child processes via `fork()`/`exec()` inheritance) connects to the daemon via the Unix socket, presenting the token in each request
5. The daemon looks up the token in the permission store and computes [effective permissions](#effective-permissions) for each request
6. When the service exits, the token registration is revoked

### Token Security

- **Securely generated**: Tokens are generated using the OS cryptographic random number generator and are unpredictable
- **Inheritable**: Child processes automatically inherit the token via standard environment inheritance, so all descendants of a service share the same permissions
- **Lifecycle-bound**: Tokens are registered before the process spawns and revoked when the service exits or the config is unloaded
- **Environment variable transport**: The token is passed via the `KEPLER_TOKEN` environment variable. `KEPLER_TOKEN` should not be copied into other environment variables or manipulated by users -- doing so may lead to bugs or security issues when the token is revoked

See [Architecture](architecture.md#token-security-implementation) for implementation details.

### Configuration

**Short form (list shorthand):**

```yaml
services:
  orchestrator:
    command: ["./orchestrator"]
    permissions:
      - start
      - stop
      - status
```

**Object form:**

```yaml
services:
  orchestrator:
    command: ["./orchestrator"]
    permissions:
      allow:                          # Rights granted to this service's token
        - start
        - stop
        - status
        - logs
      hardening: no-root              # Hardening floor for configs spawned by this process
      authorize: |                    # Optional Lua authorizer (can only deny)
        if request.action == "stop" and request.params.clean then
          return false                -- deny stop --clean
        end
        return true
```

| Field       | Type       | Default         | Description                                                                                            |
| ----------- | ---------- | --------------- | ------------------------------------------------------------------------------------------------------ |
| `allow`     | `string[]` | required        | Rights granted to the process. Subject to [permission ceiling](#permission-ceiling-token-inheritance) and [ACL intersection](#effective-permissions) at request time |
| `hardening` | `string`   | inherited       | Hardening floor for configs spawned by this process. Cannot exceed the parent config's hardening level |
| `authorize` | `string`   | none            | Lua authorizer source. Compiled at service start in the ACL Lua VM. Runs before ACL authorizers on each request -- can only deny within the token's rights. See [Lua Authorizers](#lua-authorizers) |

### Permission Lifecycle

- **Created at start**: A new token is generated for each service start or restart
- **Revoked at stop**: Tokens are automatically revoked when the service exits, or when the config is unloaded (`stop --clean`, `recreate`, `prune`)
- **Isolated per-service**: Each service gets its own independent token -- permissions cannot be shared across unrelated services
- **Inherited by children**: Child processes inherit the `KEPLER_TOKEN` environment variable and share the parent's permissions

Rights use the same set as [ACL rights](#available-rights).

### Without Permissions Field

When no `permissions` field is present, the service process has no token and no ability to authenticate to the daemon. Combined with [`kepler` group stripping](#kepler-group-stripping) (active by default with `no-root` hardening), this means the process has no daemon access at all.

---

## Effective Permissions

Kepler uses a unified authorization model where every request goes through the same gate. The effective permissions depend on the caller type (Root, Group, or Token-authenticated process) and are always bounded by the per-config ACL.

### Authorization Formula

| Caller type | Formula | Description |
|---|---|---|
| **Root** (uid 0, no registration) | `effective = *` | Root always has full access. No ACL check. |
| **Group** (kepler member, no registration) | `effective = ACL(uid, gid)` | Access controlled entirely by ACL. Config owner has implicit `*`. |
| **Token-authenticated** (process with `permissions`) | `effective = Process.allow ∩ ACL(uid, gid)` | Both the process permissions AND the ACL must grant the right. |

Where `ACL(uid, gid)` resolves to:

| Caller identity | ACL value |
|---|---|
| Root (uid 0) | `*` -- implicit, never checked |
| Config owner | `*` -- implicit, never checked |
| User/group with matching ACL rules | Union of all matching rights |
| User/group with no matching rules | `∅` (denied) |
| No `acl` section in config | `∅` (denied) |

### Group ∩ ACL

When a `kepler` group member (non-root, no token registration) sends a request, the ACL is the **sole** access control mechanism:

```
effective = ACL(uid, gid)
```

The config owner has implicit `*`, so `effective = *` (unrestricted). For other group members, the ACL rules are the only source of access.

```yaml
# alice (uid 1000) is the config owner — ACL = *
# bob (uid 1001) is in the `kepler` group
kepler:
  acl:
    users:
      bob:
        allow: [status, logs]
```

```
alice → "kepler stop web"   →  effective = *                → allowed
bob   → "kepler stop web"   →  effective = {status, logs}   → denied (needs stop)
bob   → "kepler ps"         →  effective = {status, logs}   → allowed (needs status)
```

### Process ∩ ACL

When a Token-authenticated process sends a request, both layers must agree. The process's granted rights define the **maximum the process was designed to do**. The ACL defines **what the user is allowed to do on the target config**. The effective permissions are the intersection -- neither layer can escalate beyond the other.

```
effective = Process.allow ∩ ACL(uid, gid)
```

#### Process owned by root or config owner

Root and the config owner have `ACL = *`, so the ACL side drops out:

```
effective = Process.allow ∩ * = Process.allow
```

This is the common case -- a config owner's service gets exactly the rights declared in `permissions.allow`.

#### Process owned by a non-owner user

When a non-owner user starts a config (access granted via ACL), processes spawned by that config inherit the user's identity. At request time, the process permissions are intersected with the **target** config's ACL:

```yaml
# outer.kepler.yaml — owned by alice
kepler:
  acl:
    users:
      bob:
        allow: [start, stop, restart, status]
services:
  manager:
    command: ["./manager"]
    permissions:
      allow: [start, stop, status]
```

```yaml
# inner.kepler.yaml — owned by alice
kepler:
  acl:
    users:
      bob:
        allow: [status]
```

Bob starts `outer.kepler.yaml`. The `manager` service gets `Process.allow = {start, stop, status}`. When `manager` connects:

**On `inner.kepler.yaml`** (bob's ACL is narrow):
```
effective = {start, stop, status}  ∩  {status}
          = {status}
→ can view status, cannot start/stop
```

**On `outer.kepler.yaml`** (bob's ACL is broader):
```
effective = {start, stop, status}  ∩  {start, stop, restart, status}
          = {start, stop, status}
→ process permissions are the limiting factor
```

### Permission Ceiling (Token Inheritance)

When a Token-authenticated process starts a new config that itself has services with `permissions`, a **permission ceiling** prevents privilege escalation through permission chaining.

The permissions registered for a child service are computed as:

```
child.allow = expand(service.permissions.allow) ∩ caller_ceiling
```

Where `caller_ceiling` is:

| Caller auth type | Ceiling |
|---|---|
| **Token-authenticated** | Parent process's `allow` set |
| **Root** or **Group** | No ceiling (unlimited) |

This guarantees that permissions can never grow in scope across nesting levels:

```
process_N.allow ⊆ process_(N-1).allow ⊆ ... ⊆ process_1.allow
```

#### Example: Three-level nesting

```yaml
# level1.kepler.yaml — started by alice (Group auth, no ceiling)
services:
  orchestrator:
    command: ["./orchestrator"]
    permissions:
      allow: [start, stop, restart, status]
```

```yaml
# level2.kepler.yaml — started by orchestrator's process
services:
  worker:
    command: ["./worker"]
    permissions:
      allow: [start, stop, status, inspect]
```

```yaml
# level3.kepler.yaml — started by worker's process
services:
  leaf:
    command: ["./leaf"]
    permissions:
      allow: [all]
```

Permission computation at each level:

```
orchestrator.allow = expand({start, stop, restart, status}) ∩ (no ceiling)
                   = {start, stop, restart, status}

worker.allow       = expand({start, stop, status, inspect}) ∩ orchestrator.allow
                   = {start, stop, status}
                     # inspect cut — not in orchestrator's ceiling

leaf.allow         = expand({all}) ∩ worker.allow
                   = {start, stop, status}
                     # everything beyond worker's ceiling is cut
```

Even though `leaf` declares `allow: [all]`, it can never exceed what `orchestrator` was granted. Each nesting level can only narrow, never widen.

### Hardening Floor (Process Constraint)

In addition to rights restriction, token registrations carry a **hardening floor** that constrains which hardening level child configs can use. When a Token-authenticated process starts or recreates a config:

- If `permissions.hardening` is set, the process's registration enforces it as a minimum
- The child config cannot request a lower hardening level
- If no `--hardening` flag is passed, the process's floor becomes the default

```
effective_hardening = max(daemon_floor, process_floor, requested_level)
```

The `max()` comparison uses the ordering `none < no-root < strict`.

> **Note:** When a registered process explicitly requests a hardening level below its floor, the request is **denied** (not silently raised). The `max()` formula applies only when no explicit level is requested — the process's floor becomes the default.

| Process floor | Requested | Effective | Result |
|---|---|---|---|
| `no-root` | (none) | `no-root` | Process floor used as default |
| `no-root` | `strict` | `strict` | Raised above floor -- allowed |
| `no-root` | `none` | -- | **Denied** -- below process floor |
| `none` | `strict` | `strict` | No floor -- requested level used |

```yaml
services:
  orchestrator:
    command: ["./orchestrator"]
    permissions:
      allow: [start, stop, restart, status]
      hardening: no-root
```

If the orchestrator tries `kepler start --hardening none inner.kepler.yaml`, the request is denied because `none < no-root`.

### Full Authorization Pipeline

Every request goes through an authorization pipeline. The daemon checks authorization in order: token validity, hardening constraints, filesystem access, static rights check, then Lua authorizers. If any check fails, the request is denied.

1. **Token validity** -- if a token is present, it must be registered
2. **Hardening constraints** -- privilege escalation checks
3. **Filesystem read access** -- caller must be able to read the config file
4. **Static rights check** -- `effective_rights()` then `check_rights()` against the request
5. **Lua authorizers** -- token authorizer (if any), then ACL authorizers (if any). Can only deny.

Key bypasses:
- **Root** (UID 0) bypasses ACL checks and authorizers
- **Config owner** bypasses ACL checks (implicit full access) but authorizers still run if defined
- **Group members** are checked against ACL rules, then authorizers
- **Token-authenticated processes** are checked against both their granted permissions and the ACL (intersection), then authorizers

See [Architecture](architecture.md#authorization-pipeline) for the detailed step-by-step pipeline and flowchart.

### Combined Example

Putting it all together -- ACL intersection, permission ceiling, and hardening floor:

```yaml
# deploy.kepler.yaml — owned by alice (uid 1000)
kepler:
  acl:
    users:
      bob:
        allow: [start, stop, restart, status, logs]
    groups:
      ops-team:
        allow: [status, logs]

services:
  deployer:
    command: ["./deployer"]
    permissions:
      allow: [start, stop, status]
      hardening: no-root
```

**Scenario 1: alice starts the config** (owner, Group auth)
```
alice ACL       = * (owner)
effective       = * → allowed

deployer.allow  = expand({start, stop, status}) ∩ (no ceiling)
                = {start, stop, status}
```

**Scenario 2: bob starts the config** (Group auth, ACL grants broad rights)
```
bob ACL         = {start, stop, restart, status, logs}
effective       = {start, stop, restart, status, logs} → start allowed

deployer.allow  = expand({start, stop, status}) ∩ (no ceiling)
                = {start, stop, status}
```

Bob uses Group auth (no token registration), so there is no permission ceiling. The deployer gets exactly what `permissions.allow` declares.

**Scenario 3: deployer's process operates on another config**
```yaml
# app.kepler.yaml — owned by alice
kepler:
  acl:
    users:
      bob:
        allow: [status]
```

If bob started `deploy.kepler.yaml`, the deployer runs as bob (uid 1001). When it connects to the daemon and operates on `app.kepler.yaml`:
```
effective = Process.allow ∩ ACL(bob, app.kepler)
          = {start, stop, status} ∩ {status}
          = {status}
→ can view status, cannot start/stop
```

**Scenario 4: deployer starts a child config with services**
```yaml
# child.kepler.yaml
services:
  worker:
    command: ["./worker"]
    permissions:
      allow: [all]
```

```
worker.allow = expand({all}) ∩ deployer_ceiling
             = (all rights) ∩ {start, stop, status}
             = {start, stop, status}
```

The worker cannot exceed the deployer's ceiling. And `--hardening none` would be rejected because the deployer's hardening floor is `no-root`.

---

## Hardening

The `--hardening` flag controls how strictly the daemon enforces privilege boundaries between config owners and the processes they spawn. This prevents non-root users in the `kepler` group from escalating privileges through config files.

### Hardening Levels

| Level                   | Privilege restriction                                           | Kepler group stripping                       |
| ----------------------- | --------------------------------------------------------------- | -------------------------------------------- |
| `none`                  | No restrictions                                                 | No stripping                                 |
| `no-root` (CLI default) | Non-root config owners cannot run as root (uid 0)               | Kepler group stripped from spawned processes |
| `strict`                | Non-root config owners can only run as themselves (own uid:gid) | Kepler group stripped from spawned processes |

**Privilege escalation checks** are skipped for root-owned configs (owner uid 0 or legacy configs with no owner) -- root can use any `user:` field regardless of hardening level. **Kepler group stripping** still applies to all configs when hardening is `no-root` or `strict`, including root-owned configs. This means spawned processes lose the ability to connect to the daemon socket even when the config was loaded by root.

### Usage

**Daemon-level** (sets a floor for all configs):

```bash
kepler-daemon --hardening strict    # CLI flag
```

Or via environment variable (useful for systemd units):

```bash
KEPLER_HARDENING=no-root kepler-daemon
```

The CLI flag takes precedence over the environment variable.

**Per-config** (the CLI defaults to `no-root`):

```bash
kepler start                        # Defaults to --hardening no-root
kepler start --hardening strict     # Raise to strict for this config
kepler start --hardening none       # Opt out of hardening for this config
kepler recreate --hardening strict  # Re-bake with new level
```

### Effective Hardening

The effective hardening level for a config is:

```
effective = max(daemon_hardening, config_hardening)
```

The daemon sets a floor; the CLI can raise the level per-config but never lower it. This allows administrators to enforce a baseline (e.g., `no-root`) while allowing individual configs to opt into stricter levels (e.g., `strict`).

| Daemon    | Config    | Effective |
| --------- | --------- | --------- |
| `none`    | `none`    | `none`    |
| `none`    | `no-root` | `no-root` |
| `none`    | `strict`  | `strict`  |
| `no-root` | `none`    | `no-root` |
| `no-root` | `strict`  | `strict`  |
| `strict`  | `none`    | `strict`  |
| `strict`  | `no-root` | `strict`  |

The per-config hardening level is baked into the config snapshot at load time. It persists across daemon restarts. Use `kepler recreate --hardening <level>` to change it.

### Privilege Escalation Prevention

By default (`no-root`), non-root users in the `kepler` group cannot run services as root. To opt out, pass `--hardening none` explicitly.

- **`no-root`** (default): The daemon rejects any service, hook, or health check that would resolve to uid 0 for non-root config owners.
- **`strict`**: The daemon only allows processes to run as the config owner's own uid.

The check is performed after all `${{ }}$` and `!lua` expressions are resolved, so dynamic user specs are also covered.

### Kepler Group Stripping

When hardening is `no-root` or `strict`, the `kepler` group is stripped from the supplementary groups of all spawned processes (services, hooks, and health checks). Since the CLI defaults to `no-root`, kepler group stripping is active by default.

#### Why Stripping Is Necessary

The daemon socket is owned by `root:kepler` with permissions `0o666`. Any process with `KEPLER_TOKEN` or the `kepler` group in its supplementary groups can connect to the socket. Without stripping:

1. A spawned service process inherits the `kepler` group from its parent
2. The process can connect back to the daemon socket
3. The process could load arbitrary configs, bypassing privilege restrictions
4. This creates a chain escalation path -- a non-root user's service could load a config that runs as root

Stripping the `kepler` group closes this path. Services that need to communicate with the daemon should use the [`permissions`](#service-permissions-token-based) field, which provides scoped, auditable access via token-based authentication.

#### Scope

Kepler group stripping applies to **all** configs regardless of owner -- including root-owned configs. This ensures spawned processes cannot connect back to the daemon socket to load new configs, even when the parent config was loaded by root.

Stripping is applied to:
- **Services**: supplementary groups for the spawned service process
- **Hooks**: supplementary groups for hook command processes
- **Health checks**: supplementary groups for health check command processes

#### Warning on Explicit Configuration

If a service, hook, or health check explicitly configures the `kepler` group in its `groups` list, the daemon strips it and logs a warning:

```
WARN Stripped kepler group from service 'web': spawned processes cannot inherit kepler group membership
```

This warns config authors that their explicit group configuration is being overridden by the security model.

#### Finer Control

See [Architecture](architecture.md#kepler-group-stripping-implementation) for the implementation details of group stripping.

For finer control without full hardening, you can use `os.getgroups()` in Lua to explicitly set service groups with `kepler` filtered out:

```yaml
lua: |
  -- Build supplementary groups for the config owner, excluding "kepler"
  function groups_without_kepler()
    if not owner then return {} end
    local all = os.getgroups(owner.uid)
    local filtered = {}
    for _, g in ipairs(all) do
      if g ~= "kepler" then
        table.insert(filtered, g)
      end
    end
    return filtered
  end

services:
  app:
    command: ["./app"]
    groups: ${{ groups_without_kepler() }}$
```

This gives you explicit group control per-service. With the default `no-root` hardening, `kepler` group stripping is already active, but this Lua approach gives finer-grained control when using `--hardening none`.

### Examples

#### Nested privilege escalation (blocked by default)

A user `alice` in the `kepler` group creates a config that orchestrates another kepler config:

```yaml
# escalation.kepler.yaml — loaded by alice
services:
  svc1:
    run: kepler -f inner.kepler.yaml start
    user: alice
    hooks:
      post_stop:
        - run: kepler -f inner.kepler.yaml stop --clean
      post_exit:
        - run: kepler -f inner.kepler.yaml stop --clean
```

```yaml
# inner.kepler.yaml — loaded by svc1's spawned process
services:
  svc1:
    run: whoami
    user: root
```

With the default `no-root` hardening, this is blocked by two independent defenses:

1. **Kepler group stripping** — `svc1`'s spawned process has the `kepler` group removed from its supplementary groups, so it cannot connect to the daemon socket at all:

```bash
alice$ kepler -f escalation.kepler.yaml start
[err: svc1] | Permission denied: cannot connect to daemon socket
```

2. **Privilege check** — Even if the inner config were loaded separately (e.g. via a separate CLI invocation), `user: root` is rejected for non-root config owners:

```bash
alice$ kepler -f inner.kepler.yaml start
# Error: Privilege escalation denied for service 'svc1': user 'root'
#   resolves to uid 0 (root), but config owner is uid 1000
#   (hardening level: no-root)
```

With `--hardening none`, both defenses are disabled and the escalation would succeed. This is why `none` should only be used when the security implications are understood.

#### `no-root`: block root escalation only (default)

`no-root` is the default. It prevents non-root users from running as root but allows running as other unprivileged users:

```yaml
services:
  web:
    command: ["./server"]
    user: www-data          # Allowed: www-data is not root

  worker:
    command: ["./worker"]
    user: nobody            # Allowed: nobody is not root

  setup:
    command: ["./init"]
    user: root              # BLOCKED: non-root owner cannot run as root
```

```bash
alice$ kepler start
# Error: Privilege escalation denied for service 'setup': user 'root'
#   resolves to uid 0 (root), but config owner is uid 1000
#   (hardening level: no-root)
```

#### `strict`: lock down to owner only

`strict` is the most restrictive level. Non-root config owners can only run processes as their own uid:

```yaml
services:
  web:
    command: ["./server"]
    user: alice             # Allowed: matches config owner

  worker:
    command: ["./worker"]
    user: www-data          # BLOCKED: uid 33 != owner uid 1000

  daemon:
    command: ["./daemon"]
    # No user: field — runs as config owner (alice). Always allowed.
```

```bash
alice$ kepler start --hardening strict
# Error: Privilege escalation denied for service 'worker': user 'www-data'
#   (uid 33) is not the config owner uid 1000 (hardening level: strict)
```

#### Root-owned configs: escalation unrestricted, group stripping still applies

Configs loaded by root (uid 0) skip privilege escalation checks at all hardening levels -- root can use any `user:` field:

```bash
# Root can always use any user, regardless of hardening
sudo kepler start --hardening strict    # All user: fields are allowed
```

However, `kepler` group stripping still applies to spawned processes when hardening is `no-root` or `strict`. This means processes spawned by root-owned configs cannot connect back to the daemon socket to load new configs.

#### Per-config hardening for mixed trust

Per-config hardening allows different trust levels for different configs on the same daemon:

```bash
# Trusted config — CLI default applies (no-root)
alice$ kepler -f trusted.kepler.yaml start

# Untrusted config — raise to strict for this config only
alice$ kepler -f untrusted.kepler.yaml start --hardening strict

# Fully trusted config — opt out of hardening
alice$ kepler -f dev.kepler.yaml start --hardening none
```

The trusted config can run as any non-root user, while the untrusted config is locked to alice's own uid only.

### Error Messages

When a privilege escalation is denied, the error is surfaced to the CLI user:

```
Error: Privilege escalation denied for service 'myservice': user 'root' resolves to uid 0 (root), but config owner is uid 1000 (hardening level: no-root)
```

```
Error: Privilege escalation denied for service 'myservice': user 'www-data' (uid 33) is not the config owner uid 1000 (hardening level: strict)
```

---

## Environment Isolation

By default, Kepler inherits the kepler environment (`kepler.env`) when starting services and hooks (`inherit_env: true`). This ensures services have access to `PATH` and other standard variables. Services and hooks inherit the `inherit_env` policy from `kepler.default_inherit_env` unless explicitly overridden at the service level.

For production environments where environment isolation is important, use `inherit_env: false` (globally or per-service) to prevent unintended leakage of sensitive environment variables:

- `AWS_SECRET_KEY`, `API_TOKENS`, etc. from `kepler.env` are NOT injected into the service's process environment
- Only explicitly configured `environment` entries and `env_file` variables are available
- `kepler.env` values are still available for `${{ }}$` expression evaluation — `inherit_env` only controls the process environment, not expression resolution

See [Environment Variables](environment-variables.md) for details.

---

## Lua Sandbox

Kepler's Lua scripting uses a sandboxed Luau runtime with restricted capabilities:

**Restricted:**
- No module loading (`require` blocked)
- No filesystem access (`io` library removed)
- No command execution (`os.execute` removed)
- No network access
- No debug library
- No native library loading

**Protected:**
- Environment tables (`service.env`, `service.raw_env`, `service.env_file`, `hook.env`, `hook.raw_env`, `hook.env_file`, `kepler.env`) are frozen via metatable proxies
- Writes to frozen tables raise runtime errors
- Metatables are protected from removal

Lua scripts are evaluated once during config baking -- they do not run at service runtime.

See [Lua Scripting](lua-scripting.md) for details.

---

## Log Query Security

Log filtering is gated by two sub-rights that control the query surface area available to users.

### Permission Rights

Log access uses a base right and two sub-rights:

| Right | Required for | Description |
|-------|-------------|-------------|
| `logs` | `kepler logs`, `kepler logs --follow` | Read and stream logs (no filtering) |
| `logs:search` | `kepler logs --filter "..."` | Filter logs using the search DSL. Requires `logs` base right. |
| `logs:search:sql` | `kepler logs --filter "..." --sql` | Filter logs using raw SQL WHERE clauses. Requires `logs:search`. |

**Right separation rationale:** Plain log reading (`logs`) is a read-only operation with no user-controlled SQL. DSL filtering (`logs:search`) uses a parser that only produces safe SQL fragments — users cannot write arbitrary queries. Raw SQL filtering (`logs:search:sql`) exposes the full SQL surface area and should only be granted to trusted users.

### How Filters Work

#### DSL mode (default)

When a user provides a DSL filter expression (e.g., `@service:web AND @level:error`), it is parsed by a recursive descent parser and converted to a safe SQL WHERE fragment. The parser only produces `=`, `LIKE`, `AND`, `OR`, `NOT`, `CAST`, and `json_extract` operations — users cannot inject arbitrary SQL.

See [Log Management -- Log Filtering](log-management.md#log-filtering) for the full DSL syntax reference.

#### Raw SQL mode (`--sql`)

When a user passes the `--sql` flag, the filter expression is injected as a raw SQL WHERE fragment:

```sql
SELECT ... FROM logs WHERE (level = 'err' AND service = 'web') AND id > ?1 ...
```

Values are embedded directly in the SQL string -- filters are self-contained expressions, not parameterized queries. The `?` character is rejected in filters to prevent conflicts with internal bind parameters.

Raw SQL mode requires both `logs:search` and `logs:search:sql` sub-rights.

### Defense Layers

Safety is enforced by four independent defense layers. All four are active simultaneously -- an attacker must bypass all of them to cause harm.

#### 1. Read-only connection

Log queries run on a read-only SQLite connection (`SQLITE_OPEN_READONLY`). Even if all other defenses were bypassed, the connection cannot modify data.

#### 2. SQLite authorizer (deny-by-default)

The SQLite authorizer callback is set before the query is compiled. It validates every operation in the SQL statement at **compile time** (during `prepare()`), before any data is read:

- **SELECT** on the `logs` table: allowed
- **Column reads** on `logs`: allowed
- **Whitelisted functions**: allowed (see [Allowed Functions](#allowed-functions))
- **Everything else**: denied -- writes, DDL, ATTACH, PRAGMA, reads on other tables (`sqlite_master`, etc.), non-whitelisted functions

If the authorizer denies any operation, the query fails at compile time with an error returned to the user.

#### 3. Resource limits

SQLite resource limits are tightened on the filter connection to prevent denial-of-service:

| Limit | Value | Purpose |
|-------|-------|---------|
| Expression depth | 20 | Prevents deeply nested expressions |
| LIKE pattern length | 100 bytes | Prevents expensive LIKE patterns |
| SQL length | 10,000 bytes | Prevents oversized queries |
| String length | 1,000,000 bytes | Caps result sizes |
| Compound SELECT | 2 | Prevents UNION chains |
| Function arguments | 8 | Limits function call complexity |
| Attached databases | 0 | Blocks ATTACH entirely |

Additionally, the filter string itself is validated before injection: empty filters and filters exceeding 4,096 bytes are rejected.

#### 4. Progress handler timeout

A progress handler aborts queries that run longer than 5 seconds. This is the last line of defense against queries that pass all other checks but are computationally expensive (e.g., a valid but slow `LIKE` pattern on a large dataset).

### Allowed Functions

The authorizer uses a **deny-by-default** policy for SQL functions. Only the following functions are permitted in filter expressions:

| Category | Functions |
|----------|-----------|
| JSON | `json`, `json_extract`, `json_type`, `json_valid`, `json_array_length`, `json_each`, `json_tree`, `json_object`, `json_array`, `json_quote` |
| String | `length`, `lower`, `upper`, `trim`, `ltrim`, `rtrim`, `substr`, `substring`, `replace`, `instr`, `like`, `glob` |
| Comparison | `coalesce`, `ifnull`, `iif`, `nullif`, `typeof`, `min`, `max` |
| Numeric | `abs`, `round` |
| Aggregate | `count`, `sum`, `total`, `avg`, `group_concat` |

All other functions are blocked. This prevents:
- **`randomblob()`, `zeroblob()`**: memory allocation attacks
- **`load_extension()`**: arbitrary code execution
- **`printf()`**: format string abuse
- **`readfile()`, `writefile()`**: filesystem access (if the `fileio` extension were loaded)

### Known Attack Vectors and Mitigations

#### DSL mode (`logs:search`)

The DSL parser converts user queries into a restricted set of SQL operations. Users cannot inject arbitrary SQL through the DSL — the parser only produces `=`, `LIKE`, `AND`, `OR`, `NOT`, `CAST`, and `json_extract` calls. All user-provided values are properly quoted and escaped.

**Residual risk:** Users can filter on any column in the `logs` table and search log content. This is by design. Grant `logs` without `logs:search` to users who should only view raw log output without query capabilities.

#### Raw SQL mode (`logs:search:sql`)

Operators granting `logs:search:sql` should be aware of the following residual risks and how they are mitigated:

#### Information disclosure via log content

**Risk:** A user with `logs:search:sql` can write expressive queries against all columns of the `logs` table (`id`, `timestamp`, `service`, `hook`, `level`, `line`, `attributes`). They can extract substrings, filter by patterns, and use JSON functions to drill into structured log lines.

**Mitigation:** This is by design -- `logs:search:sql` is intended for advanced ad-hoc log analysis. Grant `logs:search` (without `logs:search:sql`) for DSL-only filtering, or grant `logs` alone to users who should only view raw log output. The authorizer ensures queries cannot reach tables other than `logs`.

#### CPU exhaustion via expensive queries

**Risk:** A user can craft queries that are valid but computationally expensive -- for example, `LIKE` patterns with leading wildcards on large datasets, or deeply nested `coalesce()` calls.

**Mitigation:** The 5-second progress handler timeout aborts runaway queries. Expression depth is capped at 20. LIKE patterns are limited to 100 bytes. Each query runs on a fresh read-only connection, so an aborted query cannot affect other connections or the log writer.

#### Schema probing

**Risk:** A user might attempt to discover table structure or other tables via `sqlite_master`, `PRAGMA`, or `ATTACH`.

**Mitigation:** The authorizer denies reads on any table other than `logs`, denies all PRAGMA operations, and denies ATTACH. The attached databases limit is set to 0. Attempts to probe the schema fail at compile time.

#### Statement injection via semicolons

**Risk:** A user might attempt to inject additional SQL statements by including `;` in the filter (e.g., `1=1; DROP TABLE logs`).

**Mitigation:** SQLite's `prepare()` only compiles the **first** statement in the input. Any text after a semicolon is silently ignored -- it is never compiled or executed. Combined with the read-only connection, even if a second statement were somehow executed, it could not modify data.

#### Parameter placeholder interference

**Risk:** A user might include `?` in the filter to interfere with internal bind parameters (e.g., `id = ?1` to read the service name bound at position 1).

**Mitigation:** The `?` character is rejected during filter validation, before the filter reaches SQL. Filters are self-contained expressions with literal values only.

### Configuration Examples

#### Grant log viewing without search

```yaml
kepler:
  acl:
    groups:
      viewers:
        allow: [status, logs]              # Can view status and logs, cannot use --filter
```

#### Grant DSL log search (recommended for most users)

```yaml
kepler:
  acl:
    groups:
      analysts:
        allow: [status, logs, logs:search] # DSL filtering only, no raw SQL
```

#### Grant full log access including raw SQL

```yaml
kepler:
  acl:
    groups:
      power-users:
        allow: [status, logs, logs:search, logs:search:sql]  # DSL + raw SQL filtering
```

#### Grant everything via the all alias

```yaml
kepler:
  acl:
    groups:
      ops-team:
        allow: [all]                       # Every right including logs:search:sql
```

---

## Privilege Dropping

Services can run as specific users/groups:

- Daemon spawns `kepler-exec` wrapper (still as root)
- `kepler-exec` resolves user spec and sets supplementary groups (`initgroups` or `setgroups`)
- `kepler-exec` drops privileges via `setgid()` + `setuid()`
- Resource limits applied via `setrlimit()`
- Service process runs as the target user with correct group memberships

### Default User from CLI Invoker

When a non-root CLI user loads a config, services without an explicit `user:` field default to the CLI user's UID:GID. This is baked into the config snapshot at load time, so it persists across daemon restarts. Root CLI users see no change -- services without `user:` still run as root.

This prevents non-root `kepler` group members from accidentally running services as root. To explicitly run as root, set `user: root` or `user: "0"` (requires `--hardening none` for non-root config owners, since the default `no-root` hardening blocks this).

Hooks inherit the service's user by default, with per-hook override capability.

See [Privilege Dropping](privilege-dropping.md) for details.

---

## See Also

- [Lua Authorizers](lua-authorizers.md) -- Full authorizer API reference (function signature, params, sandbox)
- [Configuration](configuration.md) -- Full YAML schema reference (`acl`, `permissions` fields)
- [Privilege Dropping](privilege-dropping.md) -- User/group and resource limits
- [Environment Variables](environment-variables.md) -- Environment isolation
- [Lua Scripting](lua-scripting.md) -- Sandbox restrictions
- [Architecture](architecture.md#socket-security) -- Internal implementation
- [Testing](testing.md) -- Test environment setup
