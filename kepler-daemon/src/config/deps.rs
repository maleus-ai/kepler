//! Dependency configuration types

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::time::Duration;

use super::duration::{deserialize_optional_duration, serialize_optional_duration};

/// Condition that must be satisfied before a dependent service can start
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCondition {
    /// Service has started (status = Running, Healthy, or Unhealthy)
    #[default]
    ServiceStarted,
    /// Service is healthy (status = Healthy, requires healthcheck)
    ServiceHealthy,
    /// Service completed successfully (exited with code 0)
    ServiceCompletedSuccessfully,
}

/// Configuration for a single dependency (Docker Compose compatible)
///
/// Example:
/// ```yaml
/// depends_on:
///   db:
///     condition: service_healthy
///     restart: true
///   redis:
///     condition: service_started
/// ```
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct DependencyConfig {
    /// Condition that must be met before dependent can start
    #[serde(default)]
    pub condition: DependencyCondition,
    /// Optional timeout for waiting on the condition
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_duration",
        serialize_with = "serialize_optional_duration"
    )]
    pub timeout: Option<Duration>,
    /// Whether to restart this service when the dependency restarts
    /// When true, if the dependency is restarted, this service will also be restarted
    #[serde(default)]
    pub restart: bool,
}

/// A dependency entry - either simple (just a name) or extended (with config)
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum DependencyEntry {
    /// Simple form: just the service name
    Simple(String),
    /// Extended form: service name with configuration (in list form)
    Extended(HashMap<String, DependencyConfig>),
}

/// Raw depends_on format - either a list or a map (Docker Compose compatible)
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum DependsOnFormat {
    /// List format: depends_on: [service-a, { service-b: { condition: ... } }]
    List(Vec<DependencyEntry>),
    /// Map format (Docker Compose style): depends_on: { db: { condition: ..., restart: true } }
    Map(HashMap<String, DependencyConfig>),
}

/// Collection of dependencies with both simple and extended forms
/// Supports both list format and Docker Compose map format
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DependsOn(pub Vec<DependencyEntry>);

impl<'de> Deserialize<'de> for DependsOn {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let format = DependsOnFormat::deserialize(deserializer)?;
        Ok(match format {
            DependsOnFormat::List(entries) => DependsOn(entries),
            DependsOnFormat::Map(map) => {
                // Convert map to list of extended entries
                DependsOn(vec![DependencyEntry::Extended(map)])
            }
        })
    }
}

impl DependsOn {
    /// Get all dependency names (for backward compatibility)
    pub fn names(&self) -> Vec<String> {
        self.0
            .iter()
            .flat_map(|entry| match entry {
                DependencyEntry::Simple(name) => vec![name.clone()],
                DependencyEntry::Extended(map) => map.keys().cloned().collect(),
            })
            .collect()
    }

    /// Get configuration for a specific dependency
    pub fn get(&self, name: &str) -> Option<DependencyConfig> {
        for entry in &self.0 {
            match entry {
                DependencyEntry::Simple(n) if n == name => {
                    return Some(DependencyConfig::default())
                }
                DependencyEntry::Extended(map) => {
                    if let Some(config) = map.get(name) {
                        return Some(config.clone());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Check if the collection is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over (name, config) pairs
    pub fn iter(&self) -> impl Iterator<Item = (String, DependencyConfig)> + '_ {
        self.0.iter().flat_map(|entry| match entry {
            DependencyEntry::Simple(name) => {
                vec![(name.clone(), DependencyConfig::default())].into_iter()
            }
            DependencyEntry::Extended(map) => map
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
                .into_iter(),
        })
    }

    /// Check if this service should restart when a specific dependency restarts
    /// This is determined by the `restart: true` setting in the dependency config
    pub fn should_restart_on_dependency(&self, dependency_name: &str) -> bool {
        self.get(dependency_name)
            .map(|config| config.restart)
            .unwrap_or(false)
    }

    /// Get all dependencies that have restart enabled
    pub fn dependencies_with_restart(&self) -> Vec<String> {
        self.iter()
            .filter(|(_, config)| config.restart)
            .map(|(name, _)| name)
            .collect()
    }
}

impl From<Vec<String>> for DependsOn {
    fn from(names: Vec<String>) -> Self {
        DependsOn(names.into_iter().map(DependencyEntry::Simple).collect())
    }
}
