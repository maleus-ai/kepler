//! Resource limits and system environment policy configuration

use serde::Deserialize;

/// Resource limits for a service process
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    /// Memory limit (e.g., "512M", "1G", "2048K")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// CPU time limit in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time: Option<u64>,
    /// Maximum number of open file descriptors
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fds: Option<u64>,
}

/// Parse memory limit string (e.g., "512M", "1G") to bytes
pub fn parse_memory_limit(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty memory limit string".to_string());
    }

    let (num_str, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, "B"));

    if num_str.is_empty() {
        return Err(format!("Invalid number in memory limit: {}", s));
    }

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in memory limit: {}", num_str))?;

    let multiplier = match unit.to_uppercase().as_str() {
        "B" | "" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        _ => return Err(format!("Unknown memory unit: {}", unit)),
    };

    num.checked_mul(multiplier)
        .ok_or_else(|| format!("Memory limit overflows u64: {}*{}", num, multiplier))
}

/// System environment inheritance policy
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SysEnvPolicy {
    /// Clear system environment, only pass explicit environment vars (secure default)
    #[default]
    Clear,
    /// Inherit all system environment variables from the daemon process
    Inherit,
}
