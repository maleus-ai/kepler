//! Restart configuration types

use serde::Deserialize;

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
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
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
        watch: Vec<String>,
    },
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

    /// Get the watch patterns (empty for simple form)
    pub fn watch_patterns(&self) -> &[String] {
        match self {
            RestartConfig::Simple(_) => &[],
            RestartConfig::Extended { watch, .. } => watch,
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
