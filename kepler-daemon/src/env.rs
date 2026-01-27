use regex::Regex;
use std::collections::HashMap;
use std::path::Path;

use crate::config::ServiceConfig;
use crate::errors::{DaemonError, Result};

/// Expand ${VAR} references in a string using environment variables and extra env
pub fn expand_env(s: &str, extra_env: &HashMap<String, String>) -> String {
    let re = Regex::new(r"\$\{([^}]+)\}").unwrap();

    re.replace_all(s, |caps: &regex::Captures| {
        let var_name = &caps[1];
        // First check extra_env, then system env
        extra_env
            .get(var_name)
            .cloned()
            .or_else(|| std::env::var(var_name).ok())
            .unwrap_or_default()
    })
    .to_string()
}

/// Load environment variables from a .env file
pub fn load_env_file(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Err(DaemonError::EnvFileNotFound(path.to_path_buf()));
    }

    let mut env = HashMap::new();

    // Use dotenvy to parse the file
    for item in dotenvy::from_path_iter(path).map_err(|e| DaemonError::EnvFileParse {
        path: path.to_path_buf(),
        source: e,
    })? {
        match item {
            Ok((key, value)) => {
                env.insert(key, value);
            }
            Err(e) => {
                return Err(DaemonError::EnvFileParse {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        }
    }

    Ok(env)
}

/// Build the full environment for a service
///
/// Priority (highest to lowest):
/// 1. Service-defined environment variables
/// 2. Variables from env_file
/// 3. System environment variables
pub fn build_service_env(
    config: &ServiceConfig,
    config_dir: &Path,
) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();

    // Start with system environment
    for (key, value) in std::env::vars() {
        env.insert(key, value);
    }

    // Load env_file if specified
    if let Some(env_file) = &config.env_file {
        let env_path = if env_file.is_absolute() {
            env_file.clone()
        } else {
            // Resolve relative to working_dir if set, otherwise config_dir
            config
                .working_dir
                .as_ref()
                .unwrap_or(&config_dir.to_path_buf())
                .join(env_file)
        };

        if env_path.exists() {
            let file_env = load_env_file(&env_path)?;
            for (key, value) in file_env {
                env.insert(key, value);
            }
        }
        // If file doesn't exist, silently ignore (common for optional .env files)
    }

    // Apply service-defined environment with variable expansion
    for entry in &config.environment {
        if let Some((key, value)) = entry.split_once('=') {
            let expanded_value = expand_env(value, &env);
            env.insert(key.to_string(), expanded_value);
        }
    }

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_basic() {
        let mut extra = HashMap::new();
        extra.insert("FOO".to_string(), "bar".to_string());
        extra.insert("BAZ".to_string(), "qux".to_string());

        assert_eq!(expand_env("${FOO}", &extra), "bar");
        assert_eq!(expand_env("prefix_${FOO}_suffix", &extra), "prefix_bar_suffix");
        assert_eq!(expand_env("${FOO}_${BAZ}", &extra), "bar_qux");
    }

    #[test]
    fn test_expand_env_missing() {
        let extra = HashMap::new();
        assert_eq!(expand_env("${NONEXISTENT}", &extra), "");
        assert_eq!(expand_env("prefix_${NONEXISTENT}_suffix", &extra), "prefix__suffix");
    }

    #[test]
    fn test_expand_env_no_vars() {
        let extra = HashMap::new();
        assert_eq!(expand_env("no variables here", &extra), "no variables here");
    }
}
