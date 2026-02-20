use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Extract the base directory from a glob pattern.
/// Returns the portion of the path before any glob characters (* ? [ {).
fn extract_base_dir(pattern: &str) -> Option<PathBuf> {
    // Find the first glob character (byte offset, safe for UTF-8)
    let first_glob_pos = pattern.find(['*', '?', '[', '{']);

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

/// Returns true if the event kind represents a content-modifying operation
/// (create, modify, remove) that should trigger a file watcher restart.
/// Only explicit mutation events pass through. Imprecise events (`Any`)
/// and non-mutating access events (open, read, close) are filtered out
/// to prevent spurious restarts — notably in Docker environments where
/// notify's inotify backend registers `WatchMask::OPEN` by default.
fn is_content_modifying(kind: &EventKind) -> bool {
    kind.is_create() || kind.is_modify() || kind.is_remove()
}

/// Message sent when a file change is detected
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub config_path: PathBuf,
    pub service_name: String,
    pub matched_files: Vec<PathBuf>,
}

/// File watcher actor that watches for file changes and sends restart events
pub struct FileWatcherActor {
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
    suppressed: Arc<AtomicBool>,
}

impl FileWatcherActor {
    /// Create a new file watcher actor
    pub fn new(
        config_path: PathBuf,
        service_name: String,
        patterns: Vec<String>,
        working_dir: PathBuf,
        restart_tx: mpsc::Sender<FileChangeEvent>,
        suppressed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config_path,
            service_name,
            patterns,
            working_dir,
            restart_tx,
            suppressed,
        }
    }

    /// Build include and exclude GlobSets from patterns.
    /// Patterns starting with '!' are treated as exclusions.
    fn build_glob_sets(patterns: &[String]) -> Option<(GlobSet, GlobSet)> {
        if patterns.is_empty() {
            return None;
        }

        let mut include_builder = GlobSetBuilder::new();
        let mut exclude_builder = GlobSetBuilder::new();
        let mut has_include = false;

        for pattern in patterns {
            let (builder, pat) = if let Some(negated) = pattern.strip_prefix('!') {
                (&mut exclude_builder, negated)
            } else {
                has_include = true;
                (&mut include_builder, pattern.as_str())
            };

            match Glob::new(pat) {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(e) => {
                    warn!("Invalid glob pattern '{}': {}", pattern, e);
                }
            }
        }

        if !has_include {
            return None;
        }

        let include_set = include_builder.build().ok()?;
        let exclude_set = exclude_builder.build().unwrap_or_else(|_| GlobSet::empty());
        Some((include_set, exclude_set))
    }

    /// Collect all directories that need to be watched based on patterns.
    /// Only include patterns (not negated) determine which directories to watch.
    fn collect_watch_dirs(patterns: &[String], default_dir: &Path) -> Vec<PathBuf> {
        let mut dirs: HashSet<PathBuf> = HashSet::new();

        for pattern in patterns {
            // Skip negation patterns — they don't add watch directories
            if pattern.starts_with('!') {
                continue;
            }

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
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some((include_set, exclude_set)) = Self::build_glob_sets(&self.patterns) else {
            debug!("No valid patterns for file watcher, exiting");
            return Ok(());
        };

        // Collect all directories to watch
        let watch_dirs = Self::collect_watch_dirs(&self.patterns, &self.working_dir);

        info!(
            "Starting file watcher for {} with patterns: {:?}, watching dirs: {:?}",
            self.service_name, self.patterns, watch_dirs
        );

        // Create raw watcher using std channel (no debouncer — we filter + debounce ourselves)
        let (watcher_tx, watcher_rx) = std_mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event: Result<notify::Event, notify::Error>| {
                let _ = watcher_tx.send(event);
            },
            notify::Config::default(),
        )?;

        // Watch all collected directories
        for dir in &watch_dirs {
            if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                warn!("Failed to watch directory {:?}: {}", dir, e);
            }
        }

        // Create a channel for receiving debounced file paths in async context
        let (async_event_tx, mut async_event_rx) = mpsc::channel::<Vec<PathBuf>>(32);

        // Spawn blocking task to receive raw events, filter out non-mutating Access events
        // (open, read, close — the root cause of spurious restarts in Docker environments),
        // debounce by path, and forward batches of changed paths to the async side.
        //
        // Debounce uses a fixed window: 500ms after the *first* event for a given path,
        // regardless of subsequent events. This ensures events are never delayed
        // indefinitely by continuous modifications — the service handles suppression
        // during transient states.
        let working_dir = self.working_dir.clone();
        tokio::task::spawn_blocking(move || {
            let debounce_timeout = Duration::from_millis(500);
            // Maps each path to the time it was *first* seen in the current debounce cycle.
            // Subsequent events for the same path do NOT reset the timer.
            let mut event_map: HashMap<PathBuf, Instant> = HashMap::new();
            let mut debounce_deadline: Option<Instant> = None;

            loop {
                let wait = match debounce_deadline {
                    Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                    None => Duration::from_secs(1),
                };

                match watcher_rx.recv_timeout(wait) {
                    Ok(Ok(event)) => {
                        if !is_content_modifying(&event.kind) {
                            continue;
                        }
                        let now = Instant::now();
                        for path in event.paths {
                            event_map.entry(path).or_insert(now);
                        }
                        if debounce_deadline.is_none() {
                            debounce_deadline = Some(now + debounce_timeout);
                        }
                    }
                    Ok(Err(error)) => {
                        warn!("File watcher error: {:?}", error);
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        if async_event_tx.is_closed() {
                            debug!("Async receiver dropped, stopping file watcher");
                            break;
                        }
                    }
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                        debug!("Watcher channel closed");
                        break;
                    }
                }

                // Emit paths whose debounce window (from first event) has expired
                if let Some(deadline) = debounce_deadline {
                    if Instant::now() >= deadline {
                        let now = Instant::now();
                        let mut ready = Vec::new();
                        let mut remaining = HashMap::new();
                        for (path, first_seen) in event_map.drain() {
                            if now.duration_since(first_seen) >= debounce_timeout {
                                ready.push(path);
                            } else {
                                remaining.insert(path, first_seen);
                            }
                        }
                        event_map = remaining;
                        debounce_deadline = if event_map.is_empty() {
                            None
                        } else {
                            event_map.values().map(|t| *t + debounce_timeout).min()
                        };
                        if !ready.is_empty() {
                            if async_event_tx.blocking_send(ready).is_err() {
                                debug!("Async event channel closed, stopping file watcher");
                                break;
                            }
                        }
                    }
                }
            }
            // Keep watcher alive until the loop exits
            drop(watcher);
            drop(working_dir);
        });

        // Main event loop - process both file events and commands
        let mut was_suppressed = false;
        loop {
            tokio::select! {
                // Handle file change events
                Some(events) = async_event_rx.recv() => {
                    let currently_suppressed = self.suppressed.load(Ordering::Acquire);

                    // Transition from suppressed → active: drain stale events
                    if was_suppressed && !currently_suppressed {
                        debug!(
                            "File watcher for {} resuming, draining stale events",
                            self.service_name
                        );
                        while async_event_rx.try_recv().is_ok() {}
                        was_suppressed = false;
                        continue;
                    }
                    was_suppressed = currently_suppressed;

                    // Discard events while suppressed
                    if currently_suppressed {
                        debug!(
                            "File watcher for {} suppressed, discarding events",
                            self.service_name
                        );
                        continue;
                    }

                    let matched_files = self.process_file_paths(&events, &include_set, &exclude_set);
                    if !matched_files.is_empty() {
                        let event = FileChangeEvent {
                            config_path: self.config_path.clone(),
                            service_name: self.service_name.clone(),
                            matched_files,
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

                // Channel closed
                else => {
                    debug!("All channels closed, stopping file watcher");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process debounced file paths and return those that should trigger a restart.
    /// A file triggers a restart if it matches an include pattern and does NOT match any exclude pattern.
    fn process_file_paths(
        &self,
        paths: &[PathBuf],
        include_set: &GlobSet,
        exclude_set: &GlobSet,
    ) -> Vec<PathBuf> {
        let mut matched_files = Vec::new();

        for path in paths {
            let path_str = path.to_string_lossy();

            // Check absolute path
            let abs_included = include_set.is_match(path.as_os_str())
                || include_set.is_match(&*path_str);

            if abs_included {
                // Check if excluded (test both absolute and relative for exclusion)
                let abs_excluded = exclude_set.is_match(path.as_os_str())
                    || exclude_set.is_match(&*path_str);
                let rel_excluded = path
                    .strip_prefix(&self.working_dir)
                    .ok()
                    .map(|rel| {
                        exclude_set.is_match(rel.as_os_str())
                            || exclude_set.is_match(&*rel.to_string_lossy())
                    })
                    .unwrap_or(false);

                if !abs_excluded && !rel_excluded {
                    debug!(
                        "File change detected for {} (absolute match): {:?}",
                        self.service_name, path
                    );
                    matched_files.push(path.clone());
                } else {
                    debug!(
                        "File change excluded for {} by negation pattern: {:?}",
                        self.service_name, path
                    );
                }
                continue;
            }

            // Check relative path
            if let Ok(relative) = path.strip_prefix(&self.working_dir) {
                let relative_str = relative.to_string_lossy();
                let rel_included = include_set.is_match(relative.as_os_str())
                    || include_set.is_match(&*relative_str);

                if rel_included {
                    let excluded = exclude_set.is_match(relative.as_os_str())
                        || exclude_set.is_match(&*relative_str)
                        || exclude_set.is_match(path.as_os_str())
                        || exclude_set.is_match(&*path_str);

                    if !excluded {
                        debug!(
                            "File change detected for {} (relative match): {:?}",
                            self.service_name, path
                        );
                        matched_files.push(path.clone());
                    } else {
                        debug!(
                            "File change excluded for {} by negation pattern: {:?}",
                            self.service_name, path
                        );
                    }
                }
            }
        }
        matched_files
    }
}

/// Handle for a spawned file watcher, supporting suppress/resume without teardown.
pub struct FileWatcherHandle {
    task_handle: JoinHandle<()>,
    suppressed: Arc<AtomicBool>,
    patterns: Vec<String>,
    working_dir: PathBuf,
}

impl FileWatcherHandle {
    /// Suppress the watcher — events will be discarded until resumed.
    pub fn suppress(&self) {
        self.suppressed.store(true, Ordering::Release);
    }

    /// Resume the watcher — stale events accumulated during suppression
    /// are drained automatically on the next event loop iteration.
    pub fn resume(&self) {
        self.suppressed.store(false, Ordering::Release);
    }

    /// Check if this watcher can be reused for the given patterns and working directory.
    pub fn matches(&self, patterns: &[String], working_dir: &Path) -> bool {
        self.patterns == patterns && self.working_dir == working_dir
    }

    /// Abort the underlying task (full teardown).
    pub fn abort(&self) {
        self.task_handle.abort();
    }

    /// Check if the underlying task has finished.
    pub fn is_finished(&self) -> bool {
        self.task_handle.is_finished()
    }
}

/// Spawn a file watcher actor for a service
///
/// Returns a `FileWatcherHandle` for the task. The watcher can be
/// suppressed/resumed or fully cancelled by aborting the handle.
pub fn spawn_file_watcher(
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> FileWatcherHandle {
    let suppressed = Arc::new(AtomicBool::new(false));
    let actor = FileWatcherActor::new(
        config_path,
        service_name,
        patterns.clone(),
        working_dir.clone(),
        restart_tx,
        suppressed.clone(),
    );

    let task_handle = tokio::spawn(async move {
        if let Err(e) = actor.run().await {
            error!("File watcher error: {}", e);
        }
    });

    FileWatcherHandle {
        task_handle,
        suppressed,
        patterns,
        working_dir,
    }
}

#[cfg(test)]
mod tests;
