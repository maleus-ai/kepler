# Outputs

Kepler supports passing structured data between lifecycle hooks, service processes, and dependent services via output markers.

## Table of Contents

- [Overview](#overview)
- [Output Marker Format](#output-marker-format)
- [Hook Step Outputs](#hook-step-outputs)
- [Service Process Outputs](#service-process-outputs)
- [Service Output Declarations](#service-output-declarations)
- [Cross-Service Output Access](#cross-service-output-access)
- [Output Resolution Timing](#output-resolution-timing)
- [Output Storage](#output-storage)
- [Size Limits](#size-limits)
- [Restrictions](#restrictions)
- [Examples](#examples)

---

## Overview

There are two ways to capture outputs, and one way to declare them:

1. **Hook step outputs** — Add `output: <step_name>` to a hook step. Lines matching `::output::KEY=VALUE` on stdout are captured and available as `ctx.hooks.<hook_name>.outputs.<step_name>.<key>`.

2. **Service process outputs** — Set `output: true` on a service. Lines matching `::output::KEY=VALUE` on the process stdout are captured on exit and available to dependent services as `deps['<name>'].outputs.<key>`.

3. **Service output declarations** — Define an `outputs:` map on a service with `${{ }}$` expressions referencing hook outputs. These are resolved after process exit and merged with process outputs. Available to dependents as `deps['<name>'].outputs.<key>`.

---

## Output Marker Format

Output markers use the format:

```
::output::KEY=VALUE
```

Rules:
- The line must start with `::output::` followed by `KEY=VALUE`
- Must contain `=` — lines without `=` are skipped with a warning
- Split on the first `=` only (values can contain `=`, e.g. URLs)
- Empty keys are rejected; empty values are allowed
- Multiple markers with the same key: **last value wins**
- Marker lines are **filtered from logs** (not written to stdout log files)
- Regular non-marker lines continue to logs normally
- Only stdout is scanned for markers (not stderr)

Example process output:

```bash
#!/bin/bash
echo "::output::token=abc-123"
echo "::output::port=8080"
echo "Regular log output"                # ← goes to logs
echo "::output::url=http://localhost:8080"  # ← captured, not logged
```

---

## Hook Step Outputs

Add `output: <step_name>` to a hook step to enable output capture on that step's stdout.

```yaml
services:
  app:
    hooks:
      pre_start:
        - run: echo "::output::token=$(generate-token)"
          output: gen_token
        - run: echo "Using token ${{ ctx.hooks.pre_start.outputs.gen_token.token }}$"
```

- Only steps with `output:` have their stdout scanned for markers
- Steps without `output:` are not scanned (no performance cost)
- The value of `output:` becomes the step name used in the access path
- Within the same hook phase, later steps can reference earlier step outputs

### Access Pattern

```
ctx.hooks.<hook_name>.outputs.<step_name>.<key>
```

A shortcut is also available:

```
hooks.<hook_name>.outputs.<step_name>.<key>
```

Both are equivalent and work in `${{ }}$` expressions and `!lua` blocks.

---

## Service Process Outputs

Set `output: true` on a service to capture `::output::KEY=VALUE` markers from its process stdout.

```yaml
services:
  producer:
    command: ["./generate-data.sh"]
    restart: no
    output: true
```

- Process stdout is scanned for `::output::KEY=VALUE` markers
- Markers are captured when the process exits
- Automatically available to dependent services via `deps['producer'].outputs.<key>`
- No `outputs:` declaration needed for process-captured values

---

## Service Output Declarations

The optional `outputs:` map lets you declare named outputs using `${{ }}$` expressions that reference hook outputs.

```yaml
services:
  app:
    command: ["./run.sh"]
    restart: no
    hooks:
      pre_start:
        - run: echo "::output::token=$(generate-token)"
          output: setup
    outputs:
      auth_token: ${{ ctx.hooks.pre_start.outputs.setup.token }}$
```

- Resolved **after process exit** (when all hook outputs are available)
- Merged with process outputs — **declarations override process values on conflict**
- Available to dependents as `deps['app'].outputs.auth_token`

---

## Cross-Service Output Access

Dependent services access outputs via the `deps` table:

```yaml
services:
  producer:
    command: ["./produce.sh"]
    restart: no
    output: true

  consumer:
    command: ["./consume.sh"]
    depends_on:
      producer:
        condition: service_completed_successfully
    environment:
      - DATA_PORT=${{ deps.producer.outputs.port }}$
```

- `deps['<name>'].outputs.<key>` or `deps.<name>.outputs.<key>` — access any output (process or declared)
- Available in `${{ }}$` expressions and `!lua` blocks
- Read-only (frozen Lua table)
- Requires the dependency to have reached a terminal state

---

## Output Resolution Timing

Outputs are resolved in this order during a service lifecycle:

1. **Service starts** — old outputs cleared from disk
2. **Pre-start hooks run** — step outputs captured for steps with `output:`
3. **Process spawns** — stdout scanned for markers if `output: true`
4. **Process exits** — process outputs written to disk
5. **Post-exit hooks run** — step outputs captured for steps with `output:`
6. **Declared `outputs:` resolved** — `${{ }}$` expressions evaluated with full hook context
7. **Final outputs stored** — process outputs merged with declared outputs (declared win)
8. **Dependent service starts** — reads predecessor's outputs from disk

---

## Output Storage

Outputs are persisted to disk under `<config_dir>/.kepler/outputs/<service>/`:

```
.kepler/outputs/
  producer/
    hooks/
      pre_start/
        setup.json            # Hook step outputs
      post_exit/
        cleanup.json
    process.json              # Process ::output:: captures
    outputs.json              # Final merged outputs (process + declared)
```

All output files are JSON maps:

```json
{
  "token": "abc-123",
  "port": "8080"
}
```

Outputs are cleared at the start of each new service run cycle.

---

## Size Limits

Each output capture instance (per hook step, per service process) has a maximum size limit.

- **Default**: 1 MB per step/process
- **Configurable** via `kepler.output_max_size`:

```yaml
kepler:
  output_max_size: "2mb"
```

Accepted formats: `"512kb"`, `"1mb"`, `"2mb"`, etc.

When the limit is exceeded, further capture is silently discarded with a log warning. Lines already captured are retained.

---

## Restrictions

- **`output: true` and `outputs:` require `restart: no`** — Output capture is designed for one-shot services that run to completion. Services with restart policies would have ambiguous output semantics across restart cycles.

- **Global hooks do NOT support output capture** — The `output:` field is only available on service hook steps, not global hooks.

- **Markers are only captured from stdout** — Stderr is not scanned for `::output::` markers.

---

## Examples

### Hook Step Chaining

Later steps in the same hook phase can reference earlier step outputs:

```yaml
services:
  app:
    command: ["./app"]
    restart: no
    hooks:
      pre_start:
        - run: |
            echo "::output::port=8080"
            echo "::output::host=localhost"
          output: config
        - run: |
            echo "Starting on ${{ hooks.pre_start.outputs.config.host }}$:${{ hooks.pre_start.outputs.config.port }}$"
```

### Hook Outputs to Service Declarations

Promote hook outputs to service-level outputs for dependent access:

```yaml
services:
  setup:
    command: ["./setup.sh"]
    restart: no
    hooks:
      pre_start:
        - run: echo "::output::token=$(generate-token)"
          output: init
    outputs:
      auth_token: ${{ ctx.hooks.pre_start.outputs.init.token }}$

  app:
    command: ["./app"]
    depends_on:
      setup:
        condition: service_completed_successfully
    environment:
      - AUTH_TOKEN=${{ deps.setup.outputs.auth_token }}$
```

### Process Output to Dependent Access

A producer service emits outputs that a consumer reads:

```yaml
services:
  producer:
    command: ["sh", "-c", "echo '::output::port=9090' && echo '::output::secret=abc'"]
    restart: no
    output: true

  consumer:
    command: ["sh", "-c", "echo Connecting to port $PRODUCER_PORT with secret $PRODUCER_SECRET"]
    depends_on:
      producer:
        condition: service_completed_successfully
    environment:
      - PRODUCER_PORT=${{ deps.producer.outputs.port }}$
      - PRODUCER_SECRET=${{ deps.producer.outputs.secret }}$
```

### Combined Process and Declared Outputs

Process outputs and declared outputs merge, with declarations taking precedence:

```yaml
services:
  worker:
    command: ["sh", "-c", "echo '::output::raw_result=42' && echo '::output::status=done'"]
    restart: no
    output: true
    hooks:
      post_exit:
        - run: echo "::output::summary=Worker finished successfully"
          output: report
    outputs:
      result: ${{ "processed:" .. (ctx.hooks.post_exit.outputs.report.summary or "unknown") }}$
      status: override_from_declaration  # overrides process "status=done"

  aggregator:
    command: ["./aggregate"]
    depends_on:
      worker:
        condition: service_completed_successfully
    environment:
      - RAW=${{ deps.worker.outputs.raw_result }}$       # from process
      - RESULT=${{ deps.worker.outputs.result }}$         # from declaration
      - STATUS=${{ deps.worker.outputs.status }}$         # "override_from_declaration"
```

---

## See Also

- [Hooks](hooks.md) -- Hook step output capture
- [Dependencies](dependencies.md) -- Dependency output access
- [Configuration](configuration.md) -- `output`, `outputs`, `output_max_size` fields
- [Inline Expressions](variable-expansion.md) -- `${{ }}$` syntax for output access
- [Lua Scripting](lua-scripting.md) -- Lua context for outputs
