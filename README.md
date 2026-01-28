# Kepler

A process orchestrator for managing application lifecycles. Kepler provides a single global daemon that manages multiple configuration files on demand, with support for health checks, file watching, hooks, and more.

## Features

- **Global Daemon Architecture**: Single daemon instance manages multiple config files
- **On-Demand Config Loading**: Configs are loaded when first referenced
- **Service Management**: Start, stop, restart services with dependency ordering
- **Health Checks**: Docker-compatible health check configuration
- **File Watching**: Automatic service restart on file changes
- **Lifecycle Hooks**: Run commands at various lifecycle stages (init, start, stop, restart, cleanup)
- **Environment Variables**: Support for env vars and `.env` files
- **Privilege Dropping**: Run services and hooks as specific users/groups (Unix only)
- **Colored Logs**: Real-time log streaming with service-colored output
- **Persistent Logs**: Logs survive daemon restarts and are stored on disk

## Installation

### Prerequisites

- **Rust toolchain** (1.85+): Install via [rustup](https://rustup.rs/)
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

### Building from Source

1. Clone the repository:
   ```bash
   git clone https://github.com/your-org/kepler.git
   cd kepler
   ```

2. Build the release binaries:
   ```bash
   cargo build --release
   ```

3. Install the binaries (optional):
   ```bash
   # Install to ~/.cargo/bin (ensure it's in your PATH)
   cargo install --path kepler-cli

   # Or copy manually to a location in your PATH
   sudo cp target/release/kepler /usr/local/bin/
   sudo cp target/release/kepler-daemon /usr/local/bin/
   ```

### Verifying Installation

```bash
kepler --version
kepler daemon start -d
kepler daemon status
kepler daemon stop
```

### Running Tests

```bash
cargo test --workspace
```

### Best Practices for Setup

**Run as a dedicated user (recommended for production):**
```bash
# Create a dedicated user
sudo useradd -r -s /bin/false kepler

# Run the daemon as that user
sudo -u kepler kepler daemon start -d
```

**Avoid running as root:**
The daemon will warn if started as root. Instead, use the `user:` option in your config to run specific services with elevated privileges when needed.

**Using systemd (recommended for production):**
```ini
# /etc/systemd/system/kepler.service
[Unit]
Description=Kepler Process Orchestrator
After=network.target

[Service]
Type=simple
User=kepler
ExecStart=/usr/local/bin/kepler daemon start
ExecStop=/usr/local/bin/kepler daemon stop
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable kepler
sudo systemctl start kepler
```

## Quick Start

1. Create a `kepler.yaml` configuration file:

```yaml
services:
  backend:
    command: ["npm", "run", "start"]
    working_dir: ./backend
    restart: always
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:3000/health || exit 1"]
      interval: 10s
      timeout: 5s
      retries: 3
```

2. Start the global daemon:

```bash
kepler daemon start -d
```

3. Start your services:

```bash
kepler start
# Or with explicit config:
kepler -f kepler.yaml start
```

4. Check status:

```bash
kepler status           # All loaded configs
kepler -f kepler.yaml status  # Specific config
kepler daemon status    # Daemon info + loaded configs
```

5. Stop services and daemon:

```bash
kepler stop
kepler daemon stop
```

## CLI Reference

### Daemon Commands (no config required)

```bash
kepler daemon start [-d]    # Start global daemon (-d for background)
kepler daemon stop          # Stop daemon (stops all services first)
kepler daemon restart [-d]  # Restart daemon
kepler daemon status        # Show daemon info and loaded configs
```

### Service Commands (require config)

```bash
kepler start [service]      # Start all or specific service
kepler stop [service]       # Stop all or specific service
kepler restart [service]    # Restart all or specific service
kepler status               # Show status of all loaded configs
kepler logs [-f] [service]  # View logs (-f to follow)
```

### Options

- `-f, --file <FILE>`: Path to config file (defaults to `kepler.yaml` in current directory)
- `-v, --verbose`: Enable verbose output
- `--clean`: Run cleanup hooks after stopping (with `stop` command)

## Configuration

### Full Example

```yaml
hooks:
  on_init:
    run: echo "First run initialization"
  on_start:
    run: echo "Kepler started"
  on_stop:
    command: ["echo", "Kepler stopped"]
  on_cleanup:
    run: docker-compose down -v

services:
  database:
    command: ["docker", "compose", "up", "postgres"]
    user: postgres              # Run as postgres user
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 5s
      retries: 5
      start_period: 10s

  backend:
    working_dir: ./apps/backend
    command: ["npm", "run", "dev"]
    user: node                  # Run as node user
    group: developers           # Override group
    depends_on:
      - database
    environment:
      - DATABASE_URL=postgres://localhost:5432/app
    env_file: .env
    restart:
      policy: on-failure
      watch:
        - "**/*.ts"
        - "**/*.json"
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:4000/health"]
      interval: 10s
      timeout: 5s
      retries: 3
    hooks:
      on_init:
        run: npm install
      on_start:
        command: ["echo", "Backend started"]
      on_restart:
        run: echo "Backend restarting..."
      on_stop:
        run: ./cleanup.sh
        user: daemon            # Run cleanup as daemon user (elevated)

  frontend:
    working_dir: ./apps/frontend
    command: ["npm", "run", "dev"]
    user: "1000:1000"           # Run as uid:gid
    depends_on:
      - backend
    environment:
      - VITE_API_URL=${BACKEND_URL}
    restart: always
    logs:
      timestamp: true
      retention:
        on_stop: retain  # Keep logs after stopping (default: clear)
```

### Configuration Reference

#### Global Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when config is first used |
| `on_start` | Runs when kepler starts |
| `on_stop` | Runs when kepler stops |
| `on_cleanup` | Runs when `--clean` flag is used |

#### Service Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `command` | `string[]` | - | Command to run (required) |
| `working_dir` | `string` | config dir | Working directory for the service |
| `depends_on` | `string[]` | `[]` | Services that must be healthy first |
| `environment` | `string[]` | `[]` | Environment variables (`KEY=value`) |
| `env_file` | `string` | - | Path to `.env` file |
| `restart` | `string\|object` | `no` | Restart configuration (see below) |
| `healthcheck` | `object` | - | Health check configuration |
| `hooks` | `object` | - | Service-specific hooks |
| `user` | `string` | - | User to run as (Unix only) |
| `group` | `string` | - | Group override (Unix only) |
| `logs` | `object` | - | Log configuration (see below) |

#### Restart Configuration

The restart configuration supports two forms:

**Simple form** (just the policy):
```yaml
restart: always
# or
restart: no
# or
restart: on-failure
```

**Extended form** (policy + optional file watching):
```yaml
restart:
  policy: always      # Required: no | always | on-failure
  watch:              # Optional: glob patterns for file watching
    - "src/**/*.ts"
    - "**/*.json"
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `policy` | `no\|always\|on-failure` | `no` | Restart policy when process exits |
| `watch` | `string[]` | `[]` | Glob patterns for file watching |

**Restart policy values:**
- `no` (default): Never restart the service when it exits
- `always`: Always restart the service when it exits
- `on-failure`: Restart only if the service exits with a non-zero code

**File watching:**
- When `watch` patterns are specified, the service restarts when matching files change
- The `on_restart` hook runs before file-change restarts (same as process-exit restarts)
- The `on_restart` log retention policy applies to file-change restarts

> **⚠️ Constraint: `policy: no` cannot be combined with `watch`**
>
> File watching requires restart to be enabled. Using `policy: no` with `watch` patterns is invalid and will be rejected during config validation with the error:
> ```
> watch patterns require restart to be enabled (policy: always or on-failure)
> ```
>
> **Invalid configuration (will fail):**
> ```yaml
> services:
>   invalid:
>     command: ["npm", "run", "dev"]
>     restart:
>       policy: no       # ❌ Cannot use 'no' with watch patterns
>       watch:
>         - "src/**/*.ts"
> ```
>
> To use file watching, set `policy` to `always` or `on-failure`.

**Examples:**
```yaml
services:
  # Development: restart on crashes AND file changes
  frontend:
    command: ["npm", "run", "dev"]
    restart:
      policy: always
      watch:
        - "src/**/*.tsx"

  # Production: restart on crashes only (simple form)
  api:
    command: ["./api-server"]
    restart: always

  # Restart on failure OR file changes
  backend:
    command: ["cargo", "run"]
    restart:
      policy: on-failure
      watch:
        - "src/**/*.rs"

  # Never restart (default behavior)
  one-shot:
    command: ["./migrate.sh"]
    # No restart config = no restarts
```

**User format** (Unix only):
- `"username"` - resolve user by name (uses user's primary group)
- `"1000"` - numeric uid (gid defaults to same value)
- `"1000:1000"` - explicit uid:gid pair

#### Log Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `timestamp` | `bool` | `false` | Include timestamps in log output |
| `store` | `bool` or `object` | `true` | Whether to store logs to disk |
| `store.stdout` | `bool` | `true` | Store stdout logs |
| `store.stderr` | `bool` | `true` | Store stderr logs |
| `retention.on_start` | `clear\|retain` | `retain` | Log retention when service starts |
| `retention.on_stop` | `clear\|retain` | `clear` | Log retention when service stops |
| `retention.on_restart` | `clear\|retain` | `retain` | Log retention when service restarts |
| `retention.on_exit` | `clear\|retain` | `retain` | Log retention when service process exits |
| `retention.on_cleanup` | `clear\|retain` | `clear` | Log retention on cleanup |

**Log storage (`store`):**

The `store` option controls whether stdout/stderr output from services and hooks is captured and stored in the log buffer. This can be specified in two forms:

**Simple form** - enable or disable all log storage:
```yaml
logs:
  store: false  # Disable all log storage
```

**Extended form** - granular control over stdout/stderr:
```yaml
logs:
  store:
    stdout: true   # Store stdout
    stderr: false  # Don't store stderr
```

This is useful for:
- **Noisy services**: Disable logging for services that produce excessive output
- **Error-only logging**: Store only stderr to capture errors while ignoring verbose stdout
- **Performance**: Reduce memory and disk usage for high-throughput services

**Log retention values:**
- `retain`: Keep logs when the event occurs (default for `on_start`, `on_restart`, `on_exit`)
- `clear`: Clear logs when the event occurs (default for `on_stop`, `on_cleanup`)

**Inheritance priority** (lowest to highest):
1. **Built-in default** - The default value shown in the table above
2. **Global-level setting** - Setting in the top-level `logs:` block
3. **Service-level setting** - Explicit setting in the service's `logs:` block

Each level overrides the previous one. Service settings override global settings, which override built-in defaults.

**Examples:**
```yaml
# Disable all log storage globally
logs:
  store: false

# Store only stderr (errors) globally
logs:
  store:
    stdout: false
    stderr: true

services:
  noisy-service:
    command: ["./verbose-app"]
    logs:
      store: false  # Disable logs for this service only

  important-service:
    command: ["./critical-app"]
    logs:
      store:
        stdout: false  # Don't store stdout
        stderr: true   # Only store errors

  backend:
    command: ["npm", "run", "dev"]
    logs:
      retention:
        on_stop: clear   # Service override: clear instead of global retain
        # on_start: not set → inherits global or built-in default
```

#### Service Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when service is first started |
| `on_start` | Runs before service starts |
| `on_stop` | Runs before service stops |
| `on_restart` | Runs before service restarts |
| `on_exit` | Runs when service process exits |
| `on_healthcheck_success` | Runs when service becomes healthy |
| `on_healthcheck_fail` | Runs when service becomes unhealthy |

**Hook format:**
```yaml
hooks:
  on_start:
    run: echo "starting"           # Shell script format
  on_stop:
    command: ["echo", "stopping"]  # Command array format
  on_restart:
    run: ./notify.sh
    user: admin                    # Run hook as specific user
    group: developers              # Override group (Unix only)
  on_init:
    run: ./setup.sh
    working_dir: ./scripts         # Override working directory
    environment:                   # Hook-specific environment variables
      - SETUP_MODE=full
      - DEBUG=true
    env_file: .env.hooks           # Hook-specific env file
```

**Hook environment variables:**
- Hooks inherit the service's environment (including minimal system env and service env_file)
- `environment:` defines hook-specific variables that override inherited ones
- `env_file:` loads additional variables from a file (relative to working_dir)
- Environment merging priority (lowest to highest):
  1. Minimal system environment (PATH, HOME, USER, SHELL)
  2. Service's `env_file` variables
  3. Service's `environment` array
  4. Hook's `env_file` variables
  5. Hook's `environment` array

**Note:** Only essential system variables (PATH, HOME, USER, SHELL) are inherited. Use `${VAR}` syntax to reference other system variables in your `environment` arrays.

**Hook user/group behavior** (Unix only):
- By default, hooks inherit the service's `user:` and `group:` settings
- `user: daemon` runs as the kepler daemon user (for elevated privileges)
- `user: <name>` runs as a specific user
- `group: <name>` overrides the group (defaults to service's group if not set)

**Hook working directory:**
- By default, hooks run in the service's `working_dir`
- `working_dir: <path>` overrides the working directory for that hook
- Relative paths are resolved relative to the service's working_dir

#### Health Check Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `test` | `string[]` | - | Health check command |
| `interval` | `duration` | `30s` | Time between checks |
| `timeout` | `duration` | `30s` | Timeout for each check |
| `retries` | `int` | `3` | Failures before unhealthy |
| `start_period` | `duration` | `0s` | Grace period before checks start |

**Health check test format:**
- `["CMD-SHELL", "curl -f http://localhost/health"]` - run via shell
- `["CMD", "pg_isready", "-U", "postgres"]` - run command directly

**Duration format:** `100ms`, `10s`, `5m`, `1h`, `1d`

## Variable Expansion

Kepler supports `${VAR}` syntax for expanding environment variables in config values. However, **commands are NOT expanded** at config time—they are left for the shell to expand at runtime.

### What IS expanded at config load time:

| Field | Example |
|-------|---------|
| `working_dir` | `working_dir: ${PROJECT_ROOT}/app` |
| `env_file` | `env_file: ${CONFIG_DIR}/.env` |
| `user`, `group` | `user: ${SERVICE_USER}` |
| `environment` entries | `- DATABASE_URL=postgres://${DB_HOST}:${DB_PORT}/db` |
| `limits.memory` | `memory: ${MEM_LIMIT}` |
| `restart.watch` patterns | `- ${SRC_DIR}/**/*.ts` |

### What is NOT expanded (shell expands at runtime):

| Field | Why |
|-------|-----|
| `command` | Shell expands `$VAR` from process environment |
| `hooks.run` / `hooks.command` | Shell expands `$VAR` from process environment |
| `healthcheck.test` | Shell expands `$VAR` from process environment |

### Expansion context

At config load time, expansion uses:
1. System environment variables (all of them)
2. `env_file` variables (if specified) — these **override** system vars for expansion

### Passing values to commands

Instead of relying on config-time expansion in commands, pass values via the `environment` array:

```yaml
services:
  app:
    command: ["sh", "-c", "echo Hello $NAME"]  # Shell expands $NAME at runtime
    environment:
      - NAME=World                             # Injected into process env
      - DB_URL=postgres://${DB_HOST}/db        # ${DB_HOST} expanded at config time
```

This design ensures:
- Commands work as users expect (shell expansion at runtime)
- Clear separation between config-time expansion and runtime environment
- Values are passed consistently through environment variables

## Lua Scripting

Kepler supports Lua scripting (using Luau) for dynamic config generation via the `!lua` and `!lua_file` YAML tags. This enables conditional configs, environment transformation, and reusable functions.

### Basic Usage

```yaml
lua: |
  function get_port()
    return env.PORT or "8080"
  end

services:
  backend:
    command: !lua |
      return {"node", "server.js", "--port", get_port()}

    environment: !lua |
      local result = {"NODE_ENV=production"}
      if env.DEBUG then
        table.insert(result, "DEBUG=true")
      end
      return result
```

### Config Structure

| Field | Description |
|-------|-------------|
| `lua:` | Inline Lua code that runs in global scope, defines functions |
| `lua_import:` | Array of paths to external Lua files to load |
| `!lua \|` | YAML tag for inline Lua that returns a value |
| `!lua_file path` | YAML tag to load and execute a Lua file |

### Available Context

In `!lua` blocks, these variables are available:

| Variable | Type | Description |
|----------|------|-------------|
| `env` | table | Read-only table of environment variables |
| `service` | string | Current service name (nil in global hooks) |
| `hook` | string | Current hook name (nil outside hooks) |
| `global` | table | Shared mutable table for cross-block state |

The `env` table includes all system environment variables, plus any loaded from `env_file`, plus any defined in `environment` (in that order of precedence).

### Examples

**Conditional env_file:**
```yaml
services:
  api:
    env_file: !lua |
      if env.NODE_ENV == "production" then
        return "prod.env"
      end
      return "dev.env"
```

**Environment transformation:**
```yaml
services:
  backend:
    environment: !lua |
      local result = {}
      for key, value in pairs(env) do
        if string.sub(key, 1, 8) == "BACKEND_" then
          local new_key = string.sub(key, 9)  -- Remove prefix
          table.insert(result, new_key .. "=" .. value)
        end
      end
      return result
```

**Shared state between services:**
```yaml
lua: |
  global.api_port = 8080

services:
  api:
    environment: !lua |
      return {"PORT=" .. tostring(global.api_port)}

  worker:
    environment: !lua |
      return {"API_URL=http://localhost:" .. tostring(global.api_port)}
```

**Using external Lua files:**
```yaml
lua_import:
  - ./lua/helpers.lua

services:
  api:
    environment: !lua |
      return transform_env(env, {prefix = "API_"})
```

**Dynamic healthcheck:**
```yaml
services:
  api:
    healthcheck:
      test: !lua |
        local port = env.PORT or "8080"
        return {"CMD-SHELL", "curl -f http://localhost:" .. port .. "/health"}
```

### Type Conversion

| Lua Type | YAML/Config Type |
|----------|------------------|
| string | string |
| number | number (use `tostring()` for string fields) |
| boolean | boolean (use `tostring()` for string fields) |
| table (array) | array |
| table (map) | map (for most fields, use array format for `environment`) |
| nil | null |

**Note:** For `command` and `environment` arrays, return strings:
```yaml
# Correct
command: !lua |
  return {"sleep", tostring(10)}

# Incorrect (will fail)
command: !lua |
  return {"sleep", 10}
```

### Helper Functions

Functions defined in `lua:` blocks must receive `env` as a parameter if they need to access environment variables:

```yaml
lua: |
  -- Functions capture their definition environment, not the caller's
  -- So pass env explicitly when needed
  function filter_env(env_table, prefix)
    local result = {}
    for key, value in pairs(env_table) do
      if string.sub(key, 1, #prefix) == prefix then
        table.insert(result, key .. "=" .. value)
      end
    end
    return result
  end

services:
  api:
    environment: !lua |
      return filter_env(env, "API_")  -- Pass env explicitly
```

### Error Handling

Lua errors during config loading will prevent the config from loading:

```
Lua error in config '/path/to/kepler.yaml': runtime error at line 3: attempt to call nil value
```

Make sure to:
- Return the correct type for each field
- Use `tostring()` for numbers in string arrays
- Handle nil values with `or` defaults: `env.PORT or "8080"`

## Kepler Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KEPLER_DAEMON_PATH` | `~/.kepler` | Override the global state directory where the daemon stores its socket, PID file, and per-config state |

**Example:**
```bash
# Use a custom state directory
export KEPLER_DAEMON_PATH=/var/run/kepler
kepler daemon start -d
```

This is useful for:
- Running multiple isolated daemon instances
- Storing state in a different location (e.g., `/var/run` for system services)
- Testing without affecting the default daemon

## Architecture

```
~/.kepler/                 # Or $KEPLER_DAEMON_PATH if set
├── kepler.sock            # Global daemon socket
├── kepler.pid             # Global daemon PID
└── configs/               # Per-config state (hash-based)
    └── <config-hash>/
        ├── config_path    # Original config file path
        ├── state.json     # Service states
        ├── pids/          # Service PID files
        └── logs/          # Service log files
```

The global daemon:
1. Listens on `~/.kepler/kepler.sock`
2. Loads configs on-demand when first referenced
3. Each config gets its own ProcessManager, StateManager, and file watcher
4. Multiple configs can run simultaneously without interference
5. Logs are persisted to disk and survive daemon restarts

## License

MIT
