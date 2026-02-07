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
    /// Service became unhealthy (was healthy before, requires healthcheck)
    ServiceUnhealthy,
    /// Service failed (exited with non-zero code)
    ServiceFailed,
    /// Service stopped (exited normally or failed)
    ServiceStopped,
}

/// A single entry in an exit code filter — either a range string or a single integer
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
enum ExitCodeEntry {
    Range(String),
    Single(i32),
}

/// A parsed exit code range (inclusive on both ends)
#[derive(Debug, Clone)]
pub struct ExitCodeRange {
    pub start: i32,
    pub end: i32,
}

/// Filter for matching exit codes — a list of ranges/single values
#[derive(Debug, Clone, Default)]
pub struct ExitCodeFilter(pub Vec<ExitCodeRange>);

impl ExitCodeFilter {
    /// Returns true if the given code matches any range in the filter.
    /// If the filter is empty (no ranges specified), matches any code.
    pub fn matches(&self, code: i32) -> bool {
        if self.0.is_empty() {
            return true;
        }
        self.0.iter().any(|r| code >= r.start && code <= r.end)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl serde::Serialize for ExitCodeFilter {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for range in &self.0 {
            if range.start == range.end {
                seq.serialize_element(&ExitCodeEntry::Single(range.start))?;
            } else {
                seq.serialize_element(&ExitCodeEntry::Range(format!(
                    "{}:{}",
                    range.start, range.end
                )))?;
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for ExitCodeFilter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries: Vec<ExitCodeEntry> = Vec::deserialize(deserializer)?;
        let mut ranges = Vec::with_capacity(entries.len());

        for entry in entries {
            match entry {
                ExitCodeEntry::Single(n) => {
                    ranges.push(ExitCodeRange { start: n, end: n });
                }
                ExitCodeEntry::Range(s) => {
                    let parts: Vec<&str> = s.splitn(2, ':').collect();
                    if parts.len() != 2 {
                        return Err(serde::de::Error::custom(format!(
                            "invalid exit code range '{}': expected 'start:end'",
                            s
                        )));
                    }
                    let start: i32 = parts[0].trim().parse().map_err(|_| {
                        serde::de::Error::custom(format!(
                            "invalid exit code range '{}': start is not a valid integer",
                            s
                        ))
                    })?;
                    let end: i32 = parts[1].trim().parse().map_err(|_| {
                        serde::de::Error::custom(format!(
                            "invalid exit code range '{}': end is not a valid integer",
                            s
                        ))
                    })?;
                    if start > end {
                        return Err(serde::de::Error::custom(format!(
                            "invalid exit code range '{}': start ({}) > end ({})",
                            s, start, end
                        )));
                    }
                    ranges.push(ExitCodeRange { start, end });
                }
            }
        }

        Ok(ExitCodeFilter(ranges))
    }
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
    /// Optional exit code filter for service_failed and service_stopped conditions
    #[serde(default, skip_serializing_if = "ExitCodeFilter::is_empty")]
    pub exit_code: ExitCodeFilter,
    /// Whether to wait for this dependency during --wait/startup.
    /// None = use condition default (startup conditions = true, deferred = false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
}

impl DependencyCondition {
    /// Whether this condition naturally resolves during startup.
    /// Startup conditions: service_started, service_healthy, service_completed_successfully
    /// Deferred conditions: service_unhealthy, service_failed, service_stopped
    pub fn is_startup_condition(&self) -> bool {
        matches!(
            self,
            DependencyCondition::ServiceStarted
                | DependencyCondition::ServiceHealthy
                | DependencyCondition::ServiceCompletedSuccessfully
        )
    }
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
