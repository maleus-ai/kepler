//! Restart configuration types

use serde::{Deserialize, Deserializer};

use super::ConfigValue;

/// Restart policy for services (bitfield).
///
/// Flags can be combined with pipe syntax: `"on-failure|on-unhealthy"`.
/// Named aliases: `no` (0), `on-failure`, `on-success`, `on-unhealthy`, `always` (all flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy(u8);

impl RestartPolicy {
    pub const ON_FAILURE: u8 = 1;
    pub const ON_SUCCESS: u8 = 1 << 1;
    pub const ON_UNHEALTHY: u8 = 1 << 2;

    pub fn no() -> Self { Self(0) }
    pub fn on_failure() -> Self { Self(Self::ON_FAILURE) }
    pub fn on_success() -> Self { Self(Self::ON_SUCCESS) }
    pub fn on_unhealthy() -> Self { Self(Self::ON_UNHEALTHY) }
    pub fn always() -> Self { Self(Self::ON_FAILURE | Self::ON_SUCCESS | Self::ON_UNHEALTHY) }

    pub fn contains(&self, flag: u8) -> bool { self.0 & flag != 0 }
    pub fn is_no(&self) -> bool { self.0 == 0 }

    pub fn should_restart_on_exit(&self, exit_code: Option<i32>) -> bool {
        match exit_code {
            Some(0) => self.contains(Self::ON_SUCCESS),
            _ => self.contains(Self::ON_FAILURE),
        }
    }

    pub fn should_restart_on_unhealthy(&self) -> bool {
        self.contains(Self::ON_UNHEALTHY)
    }

    /// Parse a restart policy string (e.g. `"on-failure|on-unhealthy"`).
    fn parse(s: &str) -> Result<Self, String> {
        let tokens: Vec<&str> = s.split('|').map(str::trim).collect();
        let mut flags: u8 = 0;
        let mut has_no = false;

        for token in &tokens {
            match *token {
                "no" => has_no = true,
                "on-failure" => flags |= Self::ON_FAILURE,
                "on-success" => flags |= Self::ON_SUCCESS,
                "on-unhealthy" => flags |= Self::ON_UNHEALTHY,
                "always" => flags |= Self::ON_FAILURE | Self::ON_SUCCESS | Self::ON_UNHEALTHY,
                unknown => {
                    return Err(format!(
                        "unknown restart policy flag `{}`. Valid flags: no, on-failure, on-success, on-unhealthy, always",
                        unknown
                    ));
                }
            }
        }

        if has_no && flags != 0 {
            return Err("`no` cannot be combined with other restart flags".to_string());
        }

        if has_no {
            Ok(Self(0))
        } else {
            Ok(Self(flags))
        }
    }

    /// Serialize to a string representation.
    fn to_string_repr(&self) -> String {
        if self.is_no() {
            return "no".to_string();
        }
        // Use `always` alias when all three flags are set
        if self.0 == Self::ON_FAILURE | Self::ON_SUCCESS | Self::ON_UNHEALTHY {
            return "always".to_string();
        }
        let mut parts = Vec::new();
        if self.contains(Self::ON_FAILURE) {
            parts.push("on-failure");
        }
        if self.contains(Self::ON_SUCCESS) {
            parts.push("on-success");
        }
        if self.contains(Self::ON_UNHEALTHY) {
            parts.push("on-unhealthy");
        }
        parts.join("|")
    }
}

impl Default for RestartPolicy {
    fn default() -> Self { Self(0) }
}

impl serde::Serialize for RestartPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string_repr())
    }
}

impl<'de> Deserialize<'de> for RestartPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        RestartPolicy::parse(&s).map_err(serde::de::Error::custom)
    }
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
        RestartConfig::Simple(RestartPolicy::no())
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
        self.policy().should_restart_on_exit(exit_code)
    }

    /// Determine if the service should restart when healthcheck marks it unhealthy
    pub fn should_restart_on_unhealthy(&self) -> bool {
        self.policy().should_restart_on_unhealthy()
    }

    /// Check if file watching is configured
    pub fn should_restart_on_file_change(&self) -> bool {
        !self.watch_patterns().is_empty()
    }

    /// Validate the restart configuration
    pub fn validate(&self) -> std::result::Result<(), String> {
        // watch requires policy to be always or on-failure
        if !self.watch_patterns().is_empty() && self.policy().is_no() {
            return Err(
                "watch patterns require restart to be enabled \
                 (policy: always or on-failure)"
                    .to_string(),
            );
        }
        Ok(())
    }
}
