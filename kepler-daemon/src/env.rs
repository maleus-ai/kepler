use std::collections::HashMap;
use std::path::Path;

use crate::errors::{DaemonError, Result};

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

/// Insert KEY=VALUE entries from a string slice into a HashMap.
pub fn insert_env_entries(env: &mut HashMap<String, String>, entries: &[String]) {
    for entry in entries {
        if let Some((key, value)) = entry.split_once('=') {
            env.insert(key.to_string(), value.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
