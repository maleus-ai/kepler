//! Health check configuration

use serde::Deserialize;
use std::time::Duration;

use super::duration::{deserialize_duration, serialize_duration};

/// Health check configuration
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct HealthCheck {
    pub test: Vec<String>,
    #[serde(default = "default_interval", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub interval: Duration,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_start_period", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
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
