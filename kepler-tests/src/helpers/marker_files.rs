//! Hook execution verification using marker files

use kepler_daemon::config::HookCommand;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::{sleep, Instant};

/// Helper for verifying hook execution via marker files
#[derive(Clone)]
pub struct MarkerFileHelper {
    base_dir: PathBuf,
}

impl MarkerFileHelper {
    /// Create a new marker file helper with the given base directory
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// Get the path to a marker file
    pub fn marker_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{}.marker", name))
    }

    /// Create a hook command that will create a marker file
    pub fn create_marker_hook(&self, name: &str) -> HookCommand {
        let marker_path = self.marker_path(name);
        HookCommand::script(format!("touch {}", marker_path.display()))
    }

    /// Create a hook command that writes the current timestamp to a marker file
    pub fn create_timestamped_marker_hook(&self, name: &str) -> HookCommand {
        let marker_path = self.marker_path(name);
        HookCommand::script(format!("date +%s >> {}", marker_path.display()))
    }

    /// Check if a marker file exists
    pub fn marker_exists(&self, name: &str) -> bool {
        self.marker_path(name).exists()
    }

    /// Wait for a marker file to appear
    pub async fn wait_for_marker(&self, name: &str, timeout: Duration) -> bool {
        let marker_path = self.marker_path(name);
        let start = Instant::now();

        while start.elapsed() < timeout {
            if marker_path.exists() {
                return true;
            }
            sleep(Duration::from_millis(50)).await;
        }

        false
    }

    /// Wait for a marker file to appear and return its contents
    pub async fn wait_for_marker_content(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Option<String> {
        let marker_path = self.marker_path(name);
        let start = Instant::now();

        while start.elapsed() < timeout {
            if marker_path.exists()
                && let Ok(content) = std::fs::read_to_string(&marker_path)
                    && !content.is_empty() {
                        return Some(content);
                    }
            sleep(Duration::from_millis(50)).await;
        }

        None
    }

    /// Count the number of lines in a marker file (for checking how many times a hook fired)
    pub fn count_marker_lines(&self, name: &str) -> usize {
        let marker_path = self.marker_path(name);
        if let Ok(content) = std::fs::read_to_string(&marker_path) {
            content.lines().count()
        } else {
            0
        }
    }

    /// Wait until the marker file has at least the specified number of lines
    pub async fn wait_for_marker_lines(
        &self,
        name: &str,
        expected_lines: usize,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();

        while start.elapsed() < timeout {
            if self.count_marker_lines(name) >= expected_lines {
                return true;
            }
            sleep(Duration::from_millis(50)).await;
        }

        false
    }

    /// Read the content of a marker file
    pub fn read_marker(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.marker_path(name)).ok()
    }

    /// Delete a marker file
    pub fn delete_marker(&self, name: &str) {
        let _ = std::fs::remove_file(self.marker_path(name));
    }

    /// Delete all marker files in the base directory
    pub fn delete_all_markers(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "marker") {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

}

#[cfg(test)]
mod tests;
