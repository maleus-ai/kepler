use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Config {}

impl Config {
    /// Find the kepler configuration file in the current or parent directories
    pub fn find_config_file(start_dir: &Path) -> Option<PathBuf> {
        let mut current = start_dir.to_path_buf();
        loop {
            let yaml_path = current.join("kepler.yaml");
            if yaml_path.exists() {
                return Some(yaml_path);
            }

            let yml_path = current.join("kepler.yml");
            if yml_path.exists() {
                return Some(yml_path);
            }

            if !current.pop() {
                return None;
            }
        }
    }

    /// Resolve config path from CLI option or search in current directory
    pub fn resolve_config_path(file: &Option<String>) -> Result<PathBuf> {
        match file {
            Some(path) => Ok(PathBuf::from(path)),
            None => {
                let cwd = std::env::current_dir().context("Failed to get current directory")?;
                Config::find_config_file(&cwd)
                    .context("No kepler.yaml or kepler.yml found. Use -f to specify a config file.")
            }
        }
    }
}
