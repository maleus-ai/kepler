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

/// File watcher actor that watches for file changes and sends restart events
pub struct FileWatcherActor {
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
}

impl FileWatcherActor {
    /// Create a new file watcher actor
    pub fn new(
        config_path: PathBuf,
        service_name: String,
        patterns: Vec<String>,
        working_dir: PathBuf,
        restart_tx: mpsc::Sender<FileChangeEvent>,
    ) -> Self {
        Self {
            config_path,
            service_name,
            patterns,
            working_dir,
            restart_tx,
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

        // Spawn blocking task to receive from std channel and forward to tokio channel.
        // Uses recv_timeout so the thread can detect when the async side is cancelled
        // (abort() on the JoinHandle drops the async_event_rx, but spawn_blocking
        // threads cannot be cancelled — they must exit on their own).
        let working_dir = self.working_dir.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                match watcher_rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(events)) => {
                        if async_event_tx.blocking_send(events).is_err() {
                            debug!("Async event channel closed, stopping file watcher");
                            break;
                        }
                    }
                    Ok(Err(error)) => {
                        warn!("File watcher error: {:?}", error);
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        // Check if the async side is still alive
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
                    let should_restart = self.process_file_events(&events, &include_set, &exclude_set);
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

                // Channel closed
                else => {
                    debug!("All channels closed, stopping file watcher");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process file events and return true if a restart should be triggered.
    /// A file triggers a restart if it matches an include pattern and does NOT match any exclude pattern.
    fn process_file_events(
        &self,
        events: &[notify_debouncer_mini::DebouncedEvent],
        include_set: &GlobSet,
        exclude_set: &GlobSet,
    ) -> bool {
        for event in events {
            if event.kind == DebouncedEventKind::Any {
                let path = &event.path;
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
                        return true;
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
                            return true;
                        } else {
                            debug!(
                                "File change excluded for {} by negation pattern: {:?}",
                                self.service_name, path
                            );
                        }
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_mini::DebouncedEvent;

    /// Helper to build glob sets from pattern strings
    fn make_glob_sets(patterns: &[&str]) -> (GlobSet, GlobSet) {
        let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        FileWatcherActor::build_glob_sets(&patterns).unwrap()
    }

    /// Helper to create a DebouncedEvent with DebouncedEventKind::Any
    fn file_event(path: &str) -> DebouncedEvent {
        DebouncedEvent {
            path: PathBuf::from(path),
            kind: DebouncedEventKind::Any,
        }
    }

    /// Helper to create a FileWatcherActor for testing process_file_events
    fn make_actor(working_dir: &str) -> FileWatcherActor {
        let (tx, _rx) = mpsc::channel(1);
        FileWatcherActor::new(
            PathBuf::from("/tmp/kepler.yaml"),
            "test-service".to_string(),
            vec![],
            PathBuf::from(working_dir),
            tx,
        )
    }

    // --- build_glob_sets tests ---

    #[test]
    fn build_glob_sets_empty_returns_none() {
        let patterns: Vec<String> = vec![];
        assert!(FileWatcherActor::build_glob_sets(&patterns).is_none());
    }

    #[test]
    fn build_glob_sets_only_excludes_returns_none() {
        let patterns = vec!["!**/*.test.ts".to_string()];
        assert!(FileWatcherActor::build_glob_sets(&patterns).is_none());
    }

    #[test]
    fn build_glob_sets_splits_include_and_exclude() {
        let (include, exclude) = make_glob_sets(&["**/*.ts", "!**/*.test.ts"]);

        // Include matches .ts files
        assert!(include.is_match("src/app.ts"));
        assert!(include.is_match("src/app.test.ts"));

        // Exclude only matches .test.ts files
        assert!(!exclude.is_match("src/app.ts"));
        assert!(exclude.is_match("src/app.test.ts"));
    }

    #[test]
    fn build_glob_sets_multiple_excludes() {
        let (include, exclude) =
            make_glob_sets(&["src/**/*.ts", "!**/*.test.ts", "!**/*.spec.ts"]);

        assert!(include.is_match("src/app.ts"));
        assert!(exclude.is_match("src/app.test.ts"));
        assert!(exclude.is_match("src/app.spec.ts"));
        assert!(!exclude.is_match("src/app.ts"));
    }

    // --- process_file_events tests ---

    #[test]
    fn process_events_include_only() {
        let actor = make_actor("/project");
        let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
        let events = vec![file_event("/project/src/app.ts")];

        assert!(actor.process_file_events(&events, &include, &exclude));
    }

    #[test]
    fn process_events_no_match() {
        let actor = make_actor("/project");
        let (include, exclude) = make_glob_sets(&["src/**/*.ts"]);
        let events = vec![file_event("/project/src/style.css")];

        assert!(!actor.process_file_events(&events, &include, &exclude));
    }

    #[test]
    fn process_events_negation_excludes_file() {
        let actor = make_actor("/project");
        let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

        // Regular .ts file should trigger restart
        let events = vec![file_event("/project/src/app.ts")];
        assert!(actor.process_file_events(&events, &include, &exclude));

        // .test.ts file should NOT trigger restart
        let events = vec![file_event("/project/src/app.test.ts")];
        assert!(!actor.process_file_events(&events, &include, &exclude));
    }

    #[test]
    fn process_events_negation_multiple_excludes() {
        let actor = make_actor("/project");
        let (include, exclude) =
            make_glob_sets(&["src/**/*.ts", "!**/*.test.ts", "!**/*.spec.ts"]);

        assert!(actor.process_file_events(
            &[file_event("/project/src/app.ts")],
            &include,
            &exclude
        ));
        assert!(!actor.process_file_events(
            &[file_event("/project/src/app.test.ts")],
            &include,
            &exclude
        ));
        assert!(!actor.process_file_events(
            &[file_event("/project/src/app.spec.ts")],
            &include,
            &exclude
        ));
    }

    #[test]
    fn process_events_mixed_batch_excluded_file_does_not_trigger() {
        let actor = make_actor("/project");
        let (include, exclude) = make_glob_sets(&["src/**/*.ts", "!**/*.test.ts"]);

        // Batch with only excluded files should not trigger
        let events = vec![
            file_event("/project/src/foo.test.ts"),
            file_event("/project/src/bar.test.ts"),
        ];
        assert!(!actor.process_file_events(&events, &include, &exclude));

        // Batch with one included file should trigger
        let events = vec![
            file_event("/project/src/foo.test.ts"),
            file_event("/project/src/app.ts"),
        ];
        assert!(actor.process_file_events(&events, &include, &exclude));
    }

    #[test]
    fn process_events_negation_with_subdirectory_pattern() {
        let actor = make_actor("/project");
        let (include, exclude) =
            make_glob_sets(&["src/**/*.ts", "!src/generated/**"]);

        assert!(actor.process_file_events(
            &[file_event("/project/src/app.ts")],
            &include,
            &exclude
        ));
        assert!(!actor.process_file_events(
            &[file_event("/project/src/generated/types.ts")],
            &include,
            &exclude
        ));
    }

    // --- collect_watch_dirs tests ---

    #[test]
    fn collect_watch_dirs_skips_negation_patterns() {
        let patterns = vec![
            "src/**/*.ts".to_string(),
            "!**/*.test.ts".to_string(),
        ];
        let dirs = FileWatcherActor::collect_watch_dirs(&patterns, Path::new("/project"));
        // Only the include pattern contributes the default dir
        assert_eq!(dirs.len(), 1);
        assert!(dirs.contains(&PathBuf::from("/project")));
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
    let actor = FileWatcherActor::new(
        config_path,
        service_name,
        patterns,
        working_dir,
        restart_tx,
    );

    tokio::spawn(async move {
        if let Err(e) = actor.run().await {
            error!("File watcher error: {}", e);
        }
    })
}
