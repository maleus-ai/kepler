use crate::errors::{CliError, Result};
use std::path::{Path, PathBuf};

pub struct Config {}

impl Config {
    /// Find the kepler configuration file in the given directory
    pub fn find_config_file(start_dir: &Path) -> Option<PathBuf> {
        let yaml_path = start_dir.join("kepler.yaml");
        if yaml_path.exists() {
            return Some(yaml_path);
        }

        let yml_path = start_dir.join("kepler.yml");
        if yml_path.exists() {
            return Some(yml_path);
        }

        None
    }

    /// Resolve config path from CLI option or search in current directory
    pub fn resolve_config_path(file: &Option<String>) -> Result<PathBuf> {
        match file {
            Some(path) => Ok(PathBuf::from(path)),
            None => {
                let cwd = std::env::current_dir()?;
                Config::find_config_file(&cwd)
                    .ok_or_else(|| CliError::ConfigNotFound(PathBuf::from("kepler.yaml")))
            }
        }
    }
}
