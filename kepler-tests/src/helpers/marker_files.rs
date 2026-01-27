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
        HookCommand::Script {
            run: format!("touch {}", marker_path.display()),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            log_level: None,
        }
    }

    /// Create a hook command that writes the current timestamp to a marker file
    pub fn create_timestamped_marker_hook(&self, name: &str) -> HookCommand {
        let marker_path = self.marker_path(name);
        HookCommand::Script {
            run: format!("date +%s >> {}", marker_path.display()),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            log_level: None,
        }
    }

    /// Create a hook command that writes a specific value to a marker file
    pub fn create_value_marker_hook(&self, name: &str, value: &str) -> HookCommand {
        let marker_path = self.marker_path(name);
        HookCommand::Script {
            run: format!("echo '{}' >> {}", value, marker_path.display()),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            log_level: None,
        }
    }

    /// Create a hook that writes environment variable values to a marker file
    pub fn create_env_capture_hook(&self, name: &str, env_vars: &[&str]) -> HookCommand {
        let marker_path = self.marker_path(name);
        let capture_commands: Vec<String> = env_vars
            .iter()
            .map(|var| format!("echo \"{}=${{{}}}\" >> {}", var, var, marker_path.display()))
            .collect();
        HookCommand::Script {
            run: capture_commands.join(" && "),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            log_level: None,
        }
    }

    /// Create a hook with environment variables that writes them to a marker file
    pub fn create_env_capture_hook_with_env(
        &self,
        name: &str,
        env_vars: &[&str],
        environment: Vec<String>,
    ) -> HookCommand {
        let marker_path = self.marker_path(name);
        let capture_commands: Vec<String> = env_vars
            .iter()
            .map(|var| format!("echo \"{}=${{{}}}\" >> {}", var, var, marker_path.display()))
            .collect();
        HookCommand::Script {
            run: capture_commands.join(" && "),
            user: None,
            group: None,
            working_dir: None,
            environment,
            env_file: None,
            log_level: None,
        }
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
            if marker_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&marker_path) {
                    if !content.is_empty() {
                        return Some(content);
                    }
                }
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
                if path.extension().map_or(false, |ext| ext == "marker") {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    /// Get the order in which markers were created based on timestamps
    pub fn get_marker_order(&self, names: &[&str]) -> Vec<String> {
        let mut markers: Vec<(String, std::time::SystemTime)> = names
            .iter()
            .filter_map(|name| {
                let path = self.marker_path(name);
                if path.exists() {
                    std::fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|time| (name.to_string(), time))
                } else {
                    None
                }
            })
            .collect();

        markers.sort_by_key(|(_, time)| *time);
        markers.into_iter().map(|(name, _)| name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_marker_file_helper() {
        let temp_dir = TempDir::new().unwrap();
        let helper = MarkerFileHelper::new(temp_dir.path());

        // Initially marker should not exist
        assert!(!helper.marker_exists("test"));

        // Create the marker file manually
        std::fs::write(helper.marker_path("test"), "").unwrap();

        // Now it should exist
        assert!(helper.marker_exists("test"));
    }

    #[tokio::test]
    async fn test_wait_for_marker() {
        let temp_dir = TempDir::new().unwrap();
        let helper = MarkerFileHelper::new(temp_dir.path());
        let helper_clone = helper.clone();

        // Spawn a task that creates the marker after a delay
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            std::fs::write(helper_clone.marker_path("delayed"), "").unwrap();
        });

        // Wait for the marker
        let result = helper
            .wait_for_marker("delayed", Duration::from_secs(1))
            .await;
        assert!(result);
    }

    #[test]
    fn test_count_marker_lines() {
        let temp_dir = TempDir::new().unwrap();
        let helper = MarkerFileHelper::new(temp_dir.path());

        // Write multiple lines
        std::fs::write(helper.marker_path("multiline"), "line1\nline2\nline3\n").unwrap();

        assert_eq!(helper.count_marker_lines("multiline"), 3);
    }
}
