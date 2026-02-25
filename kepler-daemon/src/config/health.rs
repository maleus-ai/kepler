//! Health check configuration

use serde::Deserialize;
use std::time::Duration;

use super::ConfigValue;
use super::duration::{deserialize_duration, serialize_duration};

/// Health check configuration
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCheck {
    #[serde(default)]
    pub command: ConfigValue<Vec<ConfigValue<String>>>,
    #[serde(default, skip_serializing_if = "ConfigValue::is_static_none")]
    pub run: ConfigValue<Option<String>>,
    #[serde(default = "default_interval", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub interval: Duration,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_start_period", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub start_period: Duration,
    #[serde(default)]
    pub user: ConfigValue<Option<String>>,
    #[serde(default)]
    pub groups: ConfigValue<Vec<ConfigValue<String>>>,
    /// Controls injection of user-specific env vars (HOME/USER/LOGNAME/SHELL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_identity: Option<bool>,
}

impl HealthCheck {
    /// Check if command is present (for validation).
    pub fn has_command(&self) -> bool {
        match &self.command {
            ConfigValue::Static(v) => !v.is_empty(),
            ConfigValue::Dynamic(_) => true,
        }
    }

    /// Check if run is present (for validation).
    pub fn has_run(&self) -> bool {
        match &self.run {
            ConfigValue::Static(v) => v.is_some(),
            ConfigValue::Dynamic(_) => true,
        }
    }
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
