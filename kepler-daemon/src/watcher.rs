use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Extract the base directory from a glob pattern.
/// Returns the portion of the path before any glob characters (* ? [ {).
fn extract_base_dir(pattern: &str) -> Option<PathBuf> {
    // Find the first glob character
    let glob_chars = ['*', '?', '[', '{'];
    let first_glob_pos = pattern.chars().position(|c| glob_chars.contains(&c));

    match first_glob_pos {
        Some(pos) => {
            // Get the portion before the glob character
            let prefix = &pattern[..pos];
            // Find the last path separator before the glob
            if let Some(last_sep) = prefix.rfind('/') {
                let base_path = &pattern[..last_sep];
                if base_path.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(base_path))
                }
            } else {
                // No separator before glob, return None (use working_dir)
                None
            }
        }
        None => {
            // No glob characters - treat as literal path, get parent dir
            Path::new(pattern)
                .parent()
                .map(|p| p.to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
        }
    }
}

/// Check if a pattern is absolute (starts with /)
fn is_absolute_pattern(pattern: &str) -> bool {
    pattern.starts_with('/')
}

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

    /// Collect all directories that need to be watched based on patterns
    fn collect_watch_dirs(patterns: &[String], default_dir: &Path) -> Vec<PathBuf> {
        let mut dirs: HashSet<PathBuf> = HashSet::new();

        for pattern in patterns {
            if is_absolute_pattern(pattern) {
                // Extract base directory from absolute pattern
                if let Some(base_dir) = extract_base_dir(pattern) {
                    if base_dir.exists() {
                        dirs.insert(base_dir);
                    } else {
                        warn!("Watch pattern base directory does not exist: {:?}", base_dir);
                    }
                }
            } else {
                // Relative pattern - use default working directory
                dirs.insert(default_dir.to_path_buf());
            }
        }

        // If no directories collected, use the default
        if dirs.is_empty() {
            dirs.insert(default_dir.to_path_buf());
        }

        dirs.into_iter().collect()
    }

    /// Run the actor event loop
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(mut glob_set) = Self::build_glob_set(&self.patterns) else {
            debug!("No valid patterns for file watcher, exiting");
            return Ok(());
        };

        // Collect all directories to watch
        let watch_dirs = Self::collect_watch_dirs(&self.patterns, &self.working_dir);

        info!(
            "Starting file watcher for {} with patterns: {:?}, watching dirs: {:?}",
            self.service_name, self.patterns, watch_dirs
        );

        // Create debounced watcher using std channel
        let (watcher_tx, watcher_rx) = std_mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(500), watcher_tx)?;

        // Watch all collected directories
        for dir in &watch_dirs {
            if let Err(e) = debouncer.watcher().watch(dir, RecursiveMode::Recursive) {
                warn!("Failed to watch directory {:?}: {}", dir, e);
            }
        }

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
                        // Use try_send with overflow handling
                        match self.restart_tx.try_send(event) {
                            Ok(_) => {}
                            Err(mpsc::error::TrySendError::Full(event)) => {
                                // Channel is full - log warning and try blocking send with timeout
                                warn!(
                                    "Restart event channel near capacity for service {}, applying backpressure",
                                    self.service_name
                                );
                                let send_result = tokio::time::timeout(
                                    tokio::time::Duration::from_secs(5),
                                    self.restart_tx.send(event),
                                ).await;

                                if send_result.is_err() {
                                    error!(
                                        "Failed to send restart event for service {} - channel timeout, event dropped",
                                        self.service_name
                                    );
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                debug!("Restart channel closed, stopping watcher");
                                break;
                            }
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
                let path_str = path.to_string_lossy();

                // Check if the full absolute path matches any pattern
                if glob_set.is_match(path.as_os_str()) || glob_set.is_match(&*path_str) {
                    debug!(
                        "File change detected for {} (absolute match): {:?}",
                        self.service_name, path
                    );
                    return true;
                }

                // Also check relative to working_dir for relative patterns
                if let Ok(relative) = path.strip_prefix(&self.working_dir) {
                    let relative_str = relative.to_string_lossy();
                    if glob_set.is_match(relative.as_os_str())
                        || glob_set.is_match(&*relative_str)
                    {
                        debug!(
                            "File change detected for {} (relative match): {:?}",
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
