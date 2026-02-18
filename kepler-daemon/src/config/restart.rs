//! Restart configuration types

use serde::{Deserialize, Deserializer};

use super::ConfigValue;

/// Restart policy for services
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    No,
    Always,
    OnFailure,
}

/// Restart configuration - supports both simple and extended forms
///
/// Simple form: `restart: always`
/// Extended form: `restart: { policy: always, watch: ["*.ts"] }`
///
/// Uses a custom Deserialize impl instead of `#[serde(untagged)]` because serde's
/// Content buffering (used by untagged enums) can't handle serde_yaml tagged values
/// like `!lua`. By capturing `serde_yaml::Value` first, we avoid Content entirely.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum RestartConfig {
    /// Simple form: just the policy
    /// e.g., restart: always
    Simple(RestartPolicy),

    /// Extended form: policy + optional watch patterns
    /// e.g., restart: { policy: always, watch: ["*.ts"] }
    Extended {
        #[serde(default)]
        policy: RestartPolicy,
        #[serde(default)]
        watch: ConfigValue<Vec<ConfigValue<String>>>,
    },
}

impl<'de> Deserialize<'de> for RestartConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value =
            serde_yaml::Value::deserialize(deserializer).map_err(serde::de::Error::custom)?;
        match &value {
            serde_yaml::Value::String(_) => {
                let policy: RestartPolicy =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(RestartConfig::Simple(policy))
            }
            serde_yaml::Value::Mapping(_) => {
                #[derive(Deserialize)]
                struct ExtendedHelper {
                    #[serde(default)]
                    policy: RestartPolicy,
                    #[serde(default)]
                    watch: ConfigValue<Vec<ConfigValue<String>>>,
                }
                let helper: ExtendedHelper =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(RestartConfig::Extended {
                    policy: helper.policy,
                    watch: helper.watch,
                })
            }
            _ => Err(serde::de::Error::custom(
                "expected string or mapping for restart config",
            )),
        }
    }
}

impl Default for RestartConfig {
    fn default() -> Self {
        RestartConfig::Simple(RestartPolicy::No)
    }
}

impl RestartConfig {
    /// Get the restart policy
    pub fn policy(&self) -> &RestartPolicy {
        match self {
            RestartConfig::Simple(p) => p,
            RestartConfig::Extended { policy, .. } => policy,
        }
    }

    /// Get the watch patterns (empty for simple form or unresolved dynamic).
    ///
    /// Returns only statically-known patterns. Dynamic patterns (containing `${{ }}$`)
    /// are excluded — they are resolved at service start time via `resolve_nested_vec`.
    pub fn watch_patterns(&self) -> Vec<String> {
        match self {
            RestartConfig::Simple(_) => Vec::new(),
            RestartConfig::Extended { watch, .. } => {
                match watch.as_static() {
                    Some(items) => items.iter().filter_map(|v| v.as_static().cloned()).collect(),
                    None => Vec::new(), // Dynamic — not yet resolved
                }
            }
        }
    }

    /// Determine if the service should restart based on exit code
    pub fn should_restart_on_exit(&self, exit_code: Option<i32>) -> bool {
        match self.policy() {
            RestartPolicy::No => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => exit_code != Some(0),
        }
    }

    /// Check if file watching is configured
    pub fn should_restart_on_file_change(&self) -> bool {
        !self.watch_patterns().is_empty()
    }

    /// Validate the restart configuration
    pub fn validate(&self) -> std::result::Result<(), String> {
        // watch requires policy to be always or on-failure
        if !self.watch_patterns().is_empty() && self.policy() == &RestartPolicy::No {
            return Err(
                "watch patterns require restart to be enabled \
                 (policy: always or on-failure)"
                    .to_string(),
            );
        }
        Ok(())
    }
}
