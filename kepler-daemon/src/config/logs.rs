//! Log configuration types

use serde::Deserialize;

use super::ConfigValue;
use super::duration::parse_duration;

/// Log retention policy on service stop
#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogRetention {
    #[default]
    Clear,  // Default: clear logs on stop
    Retain, // Keep logs after stop
}

/// Log storage configuration - supports simple bool or per-stream control
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LogStoreConfig {
    /// Simple form: store: true/false
    Simple(bool),
    /// Extended form: store: { stdout: true, stderr: false }
    Extended {
        #[serde(default = "default_true")]
        stdout: bool,
        #[serde(default = "default_true")]
        stderr: bool,
    },
}

fn default_true() -> bool {
    true
}

impl Default for LogStoreConfig {
    fn default() -> Self {
        LogStoreConfig::Simple(true)
    }
}

impl LogStoreConfig {
    pub fn store_stdout(&self) -> bool {
        match self {
            LogStoreConfig::Simple(v) => *v,
            LogStoreConfig::Extended { stdout, .. } => *stdout,
        }
    }

    pub fn store_stderr(&self) -> bool {
        match self {
            LogStoreConfig::Simple(v) => *v,
            LogStoreConfig::Extended { stderr, .. } => *stderr,
        }
    }
}

/// Log retention settings for different lifecycle events
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogRetentionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stop: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_start: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_restart: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_skipped: Option<LogRetention>,
}

impl LogRetentionConfig {
    /// Validate mutual exclusivity: `on_exit` cannot be used alongside `on_success` or `on_failure`.
    pub fn validate(&self) -> Result<(), String> {
        if self.on_exit.is_some() && self.on_success.is_some() {
            return Err("'on_exit' and 'on_success' are mutually exclusive in log retention config".to_string());
        }
        if self.on_exit.is_some() && self.on_failure.is_some() {
            return Err("'on_exit' and 'on_failure' are mutually exclusive in log retention config".to_string());
        }
        Ok(())
    }
}

/// Log configuration — used at both global (kepler.logs) and per-service level.
///
/// `flush_interval` and `retention_period` are only meaningful at the global level
/// (one LogStore actor per config). At the service level they are parsed but ignored.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// Whether to store logs (default: true for both streams)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<LogStoreConfig>,
    /// Nested log retention settings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<LogRetentionConfig>,
    /// How often the writer flushes batched inserts to SQLite.
    /// Duration string: "10ms", "100ms", "1s". Default: "100ms".
    /// Only meaningful at global level (kepler.logs).
    /// Supports `!lua` and `${{ }}$` expressions.
    #[serde(default)]
    pub flush_interval: ConfigValue<Option<String>>,
    /// Time-based log retention. On config load, logs older than this are deleted.
    /// Duration string: "1h", "24h", "7d", "30d". Default: none (keep forever).
    /// Only meaningful at global level (kepler.logs).
    /// Supports `!lua` and `${{ }}$` expressions.
    #[serde(default)]
    pub retention_period: ConfigValue<Option<String>>,
    /// Maximum number of log entries buffered in memory before forcing a flush.
    /// Default: 4096. Only meaningful at global level (kepler.logs).
    /// Supports `!lua` and `${{ }}$` expressions.
    #[serde(default)]
    pub batch_size: ConfigValue<Option<usize>>,
}

impl LogConfig {
    /// Get on_stop retention from nested retention config
    pub fn get_on_stop(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_stop)
    }

    /// Get on_start retention from nested retention config
    pub fn get_on_start(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_start)
    }

    /// Get on_restart retention from nested retention config
    pub fn get_on_restart(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_restart)
    }

    /// Get on_exit retention from nested retention config
    pub fn get_on_exit(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_exit)
    }

    /// Get on_success retention from nested retention config.
    /// Falls back to `on_exit` (which is sugar for setting both on_success and on_failure).
    pub fn get_on_success(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_success.or(r.on_exit))
    }

    /// Get on_failure retention from nested retention config.
    /// Falls back to `on_exit` (which is sugar for setting both on_success and on_failure).
    pub fn get_on_failure(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_failure.or(r.on_exit))
    }

    /// Get on_skipped retention from nested retention config
    pub fn get_on_skipped(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_skipped)
    }

    /// Get flush_interval as a Duration, parsing from the static string value.
    /// Returns None if unset, dynamic (not yet resolved), or invalid.
    pub fn flush_interval(&self) -> Option<std::time::Duration> {
        self.flush_interval
            .as_static()
            .and_then(|opt| opt.as_deref())
            .and_then(|s| parse_duration(s).ok())
    }

    /// Get retention_period as a Duration, parsing from the static string value.
    /// Returns None if unset, dynamic (not yet resolved), or invalid.
    pub fn retention_period(&self) -> Option<std::time::Duration> {
        self.retention_period
            .as_static()
            .and_then(|opt| opt.as_deref())
            .and_then(|s| parse_duration(s).ok())
    }

    /// Get batch_size from the static value.
    /// Returns None if unset, dynamic (not yet resolved).
    pub fn batch_size(&self) -> Option<usize> {
        self.batch_size
            .as_static()
            .and_then(|opt| *opt)
    }
}
