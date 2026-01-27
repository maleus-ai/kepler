use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::errors::{DaemonError, Result};

/// Root configuration structure
#[derive(Debug, Clone, Deserialize)]
pub struct KeplerConfig {
    #[serde(default)]
    pub hooks: Option<GlobalHooks>,
    #[serde(default)]
    pub logs: Option<LogConfig>,
    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
}

/// Global hooks that run at daemon lifecycle events
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GlobalHooks {
    pub on_init: Option<HookCommand>,
    pub on_start: Option<HookCommand>,
    pub on_stop: Option<HookCommand>,
    pub on_cleanup: Option<HookCommand>,
}

/// Hook command - either a script or a command array
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: String,
    },
    Command {
        command: Vec<String>,
    },
}

/// Service configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthCheck>,
    #[serde(default)]
    pub watch: Vec<String>,
    #[serde(default)]
    pub hooks: Option<ServiceHooks>,
    #[serde(default)]
    pub logs: Option<LogConfig>,
}

/// Restart policy for services
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    No,
    Always,
    OnFailure,
}

/// Health check configuration
#[derive(Debug, Clone, Deserialize)]
pub struct HealthCheck {
    pub test: Vec<String>,
    #[serde(default = "default_interval", deserialize_with = "deserialize_duration")]
    pub interval: Duration,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration")]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_start_period", deserialize_with = "deserialize_duration")]
    pub start_period: Duration,
}

fn default_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_retries() -> u32 {
    3
}

fn default_start_period() -> Duration {
    Duration::from_secs(0)
}

/// Service-specific hooks
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServiceHooks {
    pub on_init: Option<HookCommand>,
    pub on_start: Option<HookCommand>,
    pub on_stop: Option<HookCommand>,
    pub on_restart: Option<HookCommand>,
    pub on_exit: Option<HookCommand>,
}

/// Log retention policy on service stop
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogRetention {
    #[default]
    Clear,  // Default: clear logs on stop
    Retain, // Keep logs after stop
}

/// Log configuration
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogConfig {
    #[serde(default)]
    pub timestamp: bool,
    #[serde(default)]
    pub on_stop: LogRetention,
    #[serde(default)]
    pub on_start: LogRetention,
    #[serde(default)]
    pub on_restart: LogRetention,
    #[serde(default)]
    pub on_cleanup: LogRetention,
}

/// Deserialize duration from string like "10s", "5m", "1h"
fn deserialize_duration<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Parse duration string (e.g., "10s", "5m", "1h", "100ms")
pub fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty duration string".to_string());
    }

    // Find where the number ends and the unit begins
    let (num_str, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, "s")); // Default to seconds if no unit

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in duration: {}", num_str))?;

    let multiplier = match unit.to_lowercase().as_str() {
        "ms" => 1,
        "s" | "" => 1000,
        "m" => 60 * 1000,
        "h" => 60 * 60 * 1000,
        "d" => 24 * 60 * 60 * 1000,
        _ => return Err(format!("Unknown duration unit: {}", unit)),
    };

    Ok(Duration::from_millis(num * multiplier))
}

impl KeplerConfig {
    /// Load configuration from a YAML file
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(path.to_path_buf())
            } else {
                DaemonError::Io(e)
            }
        })?;

        let config: KeplerConfig =
            serde_yaml::from_str(&contents).map_err(|e| DaemonError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            })?;

        // Validate the config
        config.validate(path)?;

        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self, path: &std::path::Path) -> Result<()> {
        let mut errors = Vec::new();

        // Check each service
        for (name, service) in &self.services {
            // Check command is not empty
            if service.command.is_empty() {
                errors.push(format!(
                    "Service '{}': 'command' is required and must be a non-empty array",
                    name
                ));
            }

            // Check dependencies exist
            for dep in &service.depends_on {
                if !self.services.contains_key(dep) {
                    errors.push(format!(
                        "Service '{}': depends on '{}' which is not defined",
                        name, dep
                    ));
                }
            }
        }

        if !errors.is_empty() {
            return Err(DaemonError::Config(format!(
                "Configuration errors in {}:\n  - {}",
                path.display(),
                errors.join("\n  - ")
            )));
        }

        Ok(())
    }

    /// Compute a hash of the config file contents
    pub fn compute_hash(path: &std::path::Path) -> Result<String> {
        use sha2::{Digest, Sha256};
        let contents = std::fs::read(path)?;
        let hash = Sha256::digest(&contents);
        Ok(hex::encode(hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
    }
}
