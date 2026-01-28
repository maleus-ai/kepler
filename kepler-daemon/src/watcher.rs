use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Message sent when a file change is detected
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub config_path: PathBuf,
    pub service_name: String,
}

/// Commands that can be sent to the file watcher actor
#[derive(Debug)]
pub enum WatcherCommand {
    /// Update the glob patterns to watch
    UpdatePatterns(Vec<String>),
    /// Shutdown the watcher
    Shutdown,
}

/// Handle for sending commands to a file watcher actor
#[derive(Clone)]
pub struct FileWatcherHandle {
    cmd_tx: mpsc::Sender<WatcherCommand>,
}

impl FileWatcherHandle {
    /// Update the patterns being watched
    pub async fn update_patterns(&self, patterns: Vec<String>) -> Result<(), mpsc::error::SendError<WatcherCommand>> {
        self.cmd_tx.send(WatcherCommand::UpdatePatterns(patterns)).await
    }

    /// Shutdown the watcher
    pub async fn shutdown(&self) -> Result<(), mpsc::error::SendError<WatcherCommand>> {
        self.cmd_tx.send(WatcherCommand::Shutdown).await
    }
}

/// File watcher actor that watches for file changes and sends restart events
pub struct FileWatcherActor {
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
    cmd_rx: mpsc::Receiver<WatcherCommand>,
}

impl FileWatcherActor {
    /// Create a new file watcher actor
    pub fn new(
        config_path: PathBuf,
        service_name: String,
        patterns: Vec<String>,
        working_dir: PathBuf,
        restart_tx: mpsc::Sender<FileChangeEvent>,
        cmd_rx: mpsc::Receiver<WatcherCommand>,
    ) -> Self {
        Self {
            config_path,
            service_name,
            patterns,
            working_dir,
            restart_tx,
            cmd_rx,
        }
    }

    /// Build a GlobSet from patterns
    fn build_glob_set(patterns: &[String]) -> Option<GlobSet> {
        if patterns.is_empty() {
            return None;
        }

        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            match Glob::new(pattern) {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(e) => {
                    warn!("Invalid glob pattern '{}': {}", pattern, e);
                }
            }
        }
        builder.build().ok()
    }

    /// Run the actor event loop
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(mut glob_set) = Self::build_glob_set(&self.patterns) else {
            debug!("No valid patterns for file watcher, exiting");
            return Ok(());
        };

        info!(
            "Starting file watcher for {} in {:?} with patterns: {:?}",
            self.service_name, self.working_dir, self.patterns
        );

        // Create debounced watcher using std channel
        let (watcher_tx, watcher_rx) = std_mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(500), watcher_tx)?;
        debouncer.watcher().watch(&self.working_dir, RecursiveMode::Recursive)?;

        // Create a channel for receiving file events in async context
        let (async_event_tx, mut async_event_rx) = mpsc::channel::<Vec<notify_debouncer_mini::DebouncedEvent>>(32);

        // Spawn blocking task to receive from std channel and forward to tokio channel
        let working_dir = self.working_dir.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                match watcher_rx.recv() {
                    Ok(Ok(events)) => {
                        if async_event_tx.blocking_send(events).is_err() {
                            debug!("Async event channel closed, stopping file watcher");
                            break;
                        }
                    }
                    Ok(Err(error)) => {
                        warn!("File watcher error: {:?}", error);
                    }
                    Err(_) => {
                        debug!("Watcher channel closed");
                        break;
                    }
                }
            }
            // Keep debouncer alive until the loop exits
            drop(debouncer);
            drop(working_dir);
        });

        // Main event loop - process both file events and commands
        loop {
            tokio::select! {
                // Handle file change events
                Some(events) = async_event_rx.recv() => {
                    let should_restart = self.process_file_events(&events, &glob_set);
                    if should_restart {
                        let event = FileChangeEvent {
                            config_path: self.config_path.clone(),
                            service_name: self.service_name.clone(),
                        };
                        if self.restart_tx.send(event).await.is_err() {
                            debug!("Restart channel closed, stopping watcher");
                            break;
                        }
                    }
                }

                // Handle commands
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        WatcherCommand::UpdatePatterns(new_patterns) => {
                            info!(
                                "Updating file watcher patterns for {}: {:?}",
                                self.service_name, new_patterns
                            );
                            self.patterns = new_patterns;
                            if let Some(new_glob_set) = Self::build_glob_set(&self.patterns) {
                                glob_set = new_glob_set;
                            } else {
                                debug!("No valid patterns after update, stopping watcher");
                                break;
                            }
                        }
                        WatcherCommand::Shutdown => {
                            info!("File watcher shutdown requested for {}", self.service_name);
                            break;
                        }
                    }
                }

                // Both channels closed
                else => {
                    debug!("All channels closed, stopping file watcher");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process file events and return true if a restart should be triggered
    fn process_file_events(&self, events: &[notify_debouncer_mini::DebouncedEvent], glob_set: &GlobSet) -> bool {
        for event in events {
            if event.kind == DebouncedEventKind::Any {
                let path = &event.path;
                // Get path relative to working_dir
                if let Ok(relative) = path.strip_prefix(&self.working_dir) {
                    let relative_str = relative.to_string_lossy();
                    if glob_set.is_match(relative.as_os_str())
                        || glob_set.is_match(&*relative_str)
                    {
                        debug!(
                            "File change detected for {}: {:?}",
                            self.service_name, path
                        );
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Spawn a file watcher actor for a service
///
/// Returns a JoinHandle for the task. The watcher can be cancelled by aborting the handle.
pub fn spawn_file_watcher(
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> tokio::task::JoinHandle<()> {
    // Create command channel (currently unused but available for future control)
    let (_cmd_tx, cmd_rx) = mpsc::channel::<WatcherCommand>(8);

    let actor = FileWatcherActor::new(
        config_path,
        service_name,
        patterns,
        working_dir,
        restart_tx,
        cmd_rx,
    );

    tokio::spawn(async move {
        if let Err(e) = actor.run().await {
            error!("File watcher error: {}", e);
        }
    })
}

/// Spawn a file watcher actor and return both a handle and task handle
///
/// This variant returns a FileWatcherHandle that can be used to send commands
/// to the watcher (like updating patterns or requesting shutdown).
pub fn spawn_file_watcher_with_handle(
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> (FileWatcherHandle, tokio::task::JoinHandle<()>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WatcherCommand>(8);

    let actor = FileWatcherActor::new(
        config_path,
        service_name,
        patterns,
        working_dir,
        restart_tx,
        cmd_rx,
    );

    let handle = FileWatcherHandle { cmd_tx };

    let task_handle = tokio::spawn(async move {
        if let Err(e) = actor.run().await {
            error!("File watcher error: {}", e);
        }
    });

    (handle, task_handle)
}
