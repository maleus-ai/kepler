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
        /// Specific service to start (starts all if not specified)
        service: Option<String>,
    },
    /// Stop services (requires daemon to be running)
    Stop {
        /// Specific service to stop (stops all if not specified)
        service: Option<String>,
        /// Run cleanup hooks after stopping
        #[arg(long)]
        clean: bool,
    },
    /// Restart services (requires daemon to be running)
    Restart {
        /// Specific service to restart (restarts all if not specified)
        service: Option<String>,
    },
    /// View service logs
    Logs {
        /// Specific service to view logs for (shows all if not specified)
        service: Option<String>,

        /// Follow log output (use --follow, -f is reserved for --file)
        #[arg(long)]
        follow: bool,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,
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
    /// Recreate services with fresh config (re-expands environment variables)
    ///
    /// This command stops services, clears the cached config snapshot,
    /// and starts services again with freshly-expanded environment variables.
    /// Use this when you've changed environment variables and want them
    /// to take effect without restarting the daemon.
    Recreate {
        /// Specific service to recreate (recreates all if not specified)
        service: Option<String>,
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
