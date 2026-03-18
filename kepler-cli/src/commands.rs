use clap::{Args, Subcommand};

/// Arguments shared between `start` and `run` commands
#[derive(Args, Debug)]
pub struct StartRunArgs {
    /// Services to start/run (all if none specified)
    #[arg(value_name = "SERVICE")]
    pub services: Vec<String>,
    /// Detach and return immediately (don't follow logs)
    #[arg(short, long)]
    pub detach: bool,
    /// Block until startup cluster is ready, then return (requires -d)
    #[arg(long, requires = "detach")]
    pub wait: bool,
    /// Timeout for --wait mode (e.g. "30s", "5m")
    #[arg(long, requires = "wait")]
    pub timeout: Option<String>,
    /// Skip dependency waiting and `if:` condition (requires service names)
    #[arg(long)]
    pub no_deps: bool,
    /// Override system environment variables (KEY=VALUE, repeatable)
    #[arg(short = 'e', long = "override-envs", value_name = "KEY=VALUE")]
    pub override_envs: Vec<String>,
    /// Refresh all system environment variables from the current shell
    #[arg(short = 'r', long)]
    pub refresh_env: bool,
    /// Output raw log lines without formatting (no timestamp, level, service name, color)
    #[arg(long, conflicts_with = "detach")]
    pub raw: bool,
    /// Output logs as JSONL (one JSON object per line, OTEL/Datadog compatible)
    #[arg(long, conflicts_with_all = ["detach", "raw"])]
    pub json: bool,
    /// Stop all services on unhandled failure (foreground mode)
    #[arg(long, conflicts_with = "detach")]
    pub abort_on_failure: bool,
    /// Don't stop services on unhandled failure (--wait mode only)
    #[arg(long, requires = "wait")]
    pub no_abort_on_failure: bool,
    /// Per-config hardening level (none, no-root, strict) [daemon default: no-root]
    #[arg(long)]
    pub hardening: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Manage the global daemon process (no config required)
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },

    /// Start services (requires daemon to be running)
    Start {
        #[command(flatten)]
        args: StartRunArgs,
    },
    /// Run services (ephemeral mode: always reload config fresh, no snapshot)
    Run {
        #[command(flatten)]
        args: StartRunArgs,
        /// Remove entire state dir before loading (fresh slate: clears logs and metrics)
        #[arg(long)]
        start_clean: bool,
        /// Remove entire state dir after all services exit (incompatible with -d)
        #[arg(long, conflicts_with = "detach")]
        clean: bool,
    },
    /// Stop services (requires daemon to be running)
    Stop {
        /// Services to stop (stops all if none specified)
        #[arg(value_name = "SERVICE")]
        services: Vec<String>,
        /// Run cleanup hooks after stopping
        #[arg(long)]
        clean: bool,
        /// Signal to send (e.g., SIGKILL, TERM, 9). Default: SIGTERM
        #[arg(short, long)]
        signal: Option<String>,
    },
    /// Restart services (preserves config, runs restart hooks)
    Restart {
        /// Services to restart (restarts all running services if none specified)
        #[arg(value_name = "SERVICE")]
        services: Vec<String>,
        /// Wait until all restarted services are ready, then return
        #[arg(long, conflicts_with = "follow")]
        wait: bool,
        /// Timeout for --wait mode (e.g. "30s", "5m")
        #[arg(long, requires = "wait")]
        timeout: Option<String>,
        /// Output raw log lines without formatting (no timestamp, level, service name, color)
        #[arg(long)]
        raw: bool,
        /// Follow logs after restart (Ctrl+C exits log following, services keep running)
        #[arg(long, conflicts_with = "wait")]
        follow: bool,
        /// Skip dependency ordering (use specified order instead; requires service names)
        #[arg(long)]
        no_deps: bool,
        /// Override system environment variables (KEY=VALUE, repeatable)
        #[arg(short = 'e', long = "override-envs", value_name = "KEY=VALUE")]
        override_envs: Vec<String>,
        /// Refresh all system environment variables from the current shell
        #[arg(short = 'r', long)]
        refresh_env: bool,
    },
    /// Recreate config (stop, re-bake config snapshot, start)
    Recreate {
        /// Per-config hardening level (none, no-root, strict) [daemon default: no-root]
        #[arg(long)]
        hardening: Option<String>,
    },
    /// View service logs
    Logs {
        /// Services to view logs for (shows all if none specified)
        #[arg(value_name = "SERVICE")]
        services: Vec<String>,

        /// Follow log output (use --follow, -f is reserved for --file)
        #[arg(long)]
        follow: bool,

        /// Show first N lines, oldest first (default: 100)
        #[arg(long, conflicts_with = "tail")]
        head: Option<usize>,

        /// Show last N lines, newest last (default: 100)
        #[arg(long)]
        tail: Option<usize>,

        /// Exclude hook logs (pre_start, post_stop, etc.)
        #[arg(long)]
        no_hook: bool,

        /// Output raw log lines without formatting (no timestamp, level, service name, color)
        #[arg(long)]
        raw: bool,

        /// Output logs as JSONL (one JSON object per line, OTEL/Datadog compatible)
        #[arg(long, conflicts_with = "raw")]
        json: bool,

        /// Filter logs using a search expression.
        ///
        /// Supports full-text search, field matching, boolean operators,
        /// wildcards, and JSON attribute queries.
        ///
        /// Examples:
        ///   -F 'error'                           Full-text search
        ///   -F '@service:web AND @level:error'    Field matching with AND
        ///   -F '@level:error OR @level:warn'      OR operator
        ///   -F '-@service:worker'                 Negation
        ///   -F '@http.status:>400'                JSON attribute comparison
        ///   -F '"connection timeout"'             Exact phrase search
        ///   -F '@service:web*'                    Wildcard matching
        #[arg(long, short = 'F', verbatim_doc_comment, allow_hyphen_values = true)]
        filter: Option<String>,

        /// Treat --filter as a raw SQL WHERE clause instead of DSL.
        ///
        /// Requires the 'logs:search:sql' permission.
        #[arg(long, requires = "filter")]
        sql: bool,
    },
    /// List all services and their states
    PS {
        /// Show status for all loaded configs
        #[arg(short, long)]
        all: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect config and service state (JSON output)
    Inspect,
    /// Live resource monitor (CPU, memory) for services
    Top {
        /// Service to monitor
        #[arg(value_name = "SERVICE")]
        service: Option<String>,
        /// Output as JSON instead of interactive TUI
        #[arg(long)]
        json: bool,
        /// Time range for historical data (e.g. "5m", "1h", "24h")
        #[arg(long)]
        history: Option<String>,
        /// TUI refresh interval (e.g. "2s", "500ms"). Default: 2s
        #[arg(long)]
        interval: Option<String>,

        /// Filter metrics using a search expression.
        ///
        /// Supports field matching, boolean operators, comparisons, and wildcards.
        ///
        /// Examples:
        ///   -F '@service:web'                  Service name match
        ///   -F '@cpu_percent:>50'               CPU usage above 50%
        ///   -F '@memory_rss:>1000000'           RSS above 1MB
        ///   -F '@service:web AND @cpu_percent:>10'  Combined filter
        ///   -F '@service:web*'                  Wildcard matching
        #[arg(long, short = 'F', verbatim_doc_comment, allow_hyphen_values = true)]
        filter: Option<String>,

        /// Treat --filter as a raw SQL WHERE clause instead of DSL.
        ///
        /// Requires the 'monitor:search:sql' permission.
        #[arg(long, requires = "filter")]
        sql: bool,
    },
    /// Prune all stopped/orphaned config state directories
    Prune {
        /// Force prune even if services appear running
        #[arg(long)]
        force: bool,
        /// Show what would be pruned without deleting
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommands {
    /// Start the global daemon (no config required)
    Start {
        /// Detach and run in background
        #[arg(short, long)]
        detach: bool,
    },

    /// Stop the global daemon (stops all services first)
    Stop,

    /// Restart the global daemon
    Restart {
        /// Detach and run in background after restart
        #[arg(short, long)]
        detach: bool,
    },

    /// Show global daemon status and loaded configs
    Status,
}
