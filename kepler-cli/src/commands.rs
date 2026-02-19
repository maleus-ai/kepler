use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Manage the global daemon process (no config required)
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },

    /// Start services (requires daemon to be running)
    Start {
        /// Services to start (starts all if none specified)
        #[arg(value_name = "SERVICE")]
        services: Vec<String>,
        /// Detach and return immediately (don't follow logs)
        #[arg(short, long)]
        detach: bool,
        /// Block until startup cluster is ready, then return (requires -d)
        #[arg(long, requires = "detach")]
        wait: bool,
        /// Timeout for --wait mode (e.g. "30s", "5m")
        #[arg(long, requires = "wait")]
        timeout: Option<String>,
        /// Skip dependency waiting and `if:` condition (requires service names)
        #[arg(long)]
        no_deps: bool,
        /// Override system environment variables (KEY=VALUE, repeatable)
        #[arg(short = 'e', long = "override-envs", value_name = "KEY=VALUE")]
        override_envs: Vec<String>,
        /// Refresh all system environment variables from the current shell
        #[arg(short = 'r', long)]
        refresh_env: bool,
    },
    /// Stop services (requires daemon to be running)
    Stop {
        /// Specific service to stop (stops all if not specified)
        service: Option<String>,
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
        /// Detach and return immediately (don't follow logs)
        #[arg(short, long)]
        detach: bool,
        /// Block until restart is complete, then return (requires -d)
        #[arg(long, requires = "detach")]
        wait: bool,
        /// Timeout for --wait mode (e.g. "30s", "5m")
        #[arg(long, requires = "wait")]
        timeout: Option<String>,
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
    Recreate,
    /// View service logs
    Logs {
        /// Specific service to view logs for (shows all if not specified)
        service: Option<String>,

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
    },
    /// List all services and their states
    PS {
        /// Show status for all loaded configs
        #[arg(short, long)]
        all: bool,
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
