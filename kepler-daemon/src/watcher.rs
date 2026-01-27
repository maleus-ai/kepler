use globset::{Glob, GlobSetBuilder};
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

/// Spawn a file watcher for a service
pub fn spawn_file_watcher(
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) =
            file_watcher_loop(config_path, service_name, patterns, working_dir, restart_tx).await
        {
            error!("File watcher error: {}", e);
        }
    })
}

async fn file_watcher_loop(
    config_path: PathBuf,
    service_name: String,
    patterns: Vec<String>,
    working_dir: PathBuf,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if patterns.is_empty() {
        return Ok(());
    }

    // Build glob set from patterns
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                warn!("Invalid glob pattern '{}': {}", pattern, e);
            }
        }
    }
    let glob_set = builder.build()?;

    info!(
        "Starting file watcher for {} in {:?} with patterns: {:?}",
        service_name, working_dir, patterns
    );

    // Create debounced watcher using std channel
    let (tx, rx) = std_mpsc::channel();

    let mut debouncer = new_debouncer(Duration::from_millis(500), tx)?;

    debouncer.watcher().watch(&working_dir, RecursiveMode::Recursive)?;

    // Process events in a blocking task
    let config_path_clone = config_path.clone();
    let service_name_clone = service_name.clone();

    tokio::task::spawn_blocking(move || {
        loop {
            match rx.recv() {
                Ok(Ok(events)) => {
                    // Check if any event matches our patterns
                    let mut should_restart = false;

                    for event in events {
                        if event.kind == DebouncedEventKind::Any {
                            let path = &event.path;
                            // Get path relative to working_dir
                            if let Ok(relative) = path.strip_prefix(&working_dir) {
                                let relative_str = relative.to_string_lossy();
                                if glob_set.is_match(relative.as_os_str())
                                    || glob_set.is_match(&*relative_str)
                                {
                                    debug!(
                                        "File change detected for {}: {:?}",
                                        service_name_clone, path
                                    );
                                    should_restart = true;
                                    break;
                                }
                            }
                        }
                    }

                    if should_restart {
                        let event = FileChangeEvent {
                            config_path: config_path_clone.clone(),
                            service_name: service_name_clone.clone(),
                        };

                        if restart_tx.blocking_send(event).is_err() {
                            debug!("Restart channel closed, stopping watcher");
                            break;
                        }
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
    });

    // Keep the task alive until cancelled
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
