//! Output storage for hook step and service process outputs.
//!
//! Disk layout under `<config_dir>/.kepler/outputs/<service>/`:
//! - `hooks/<hook_name>/<step_name>.json` — hook step outputs
//! - `process.json` — process `::output::KEY=VALUE` outputs
//! - `outputs.json` — final combined outputs for dependent services

use std::collections::HashMap;
use std::path::Path;

use crate::errors::Result;

/// Parse raw `KEY=VALUE` capture lines into a HashMap.
/// Duplicate keys: last value wins.
pub fn parse_capture_lines(lines: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            if !key.is_empty() {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

/// Base directory for a service's outputs.
fn service_outputs_dir(config_dir: &Path, service: &str) -> std::path::PathBuf {
    config_dir
        .join(".kepler")
        .join("outputs")
        .join(service)
}

/// Write hook step outputs for a service hook phase/step.
pub fn write_hook_step_outputs(
    config_dir: &Path,
    service: &str,
    hook_name: &str,
    step_name: &str,
    outputs: &HashMap<String, String>,
) -> Result<()> {
    let dir = service_outputs_dir(config_dir, service)
        .join("hooks")
        .join(hook_name);
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to create hook output dir {:?}: {}",
            dir, e
        ))
    })?;
    let path = dir.join(format!("{}.json", step_name));
    let json = serde_json::to_string(outputs).map_err(|e| {
        crate::errors::DaemonError::Internal(format!("Failed to serialize hook outputs: {}", e))
    })?;
    std::fs::write(&path, json).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to write hook outputs to {:?}: {}",
            path, e
        ))
    })?;
    Ok(())
}

/// Read all hook outputs for a service: `hook_name -> step_name -> { key -> value }`.
pub fn read_all_hook_outputs(
    config_dir: &Path,
    service: &str,
) -> HashMap<String, HashMap<String, HashMap<String, String>>> {
    let hooks_dir = service_outputs_dir(config_dir, service).join("hooks");
    let mut result: HashMap<String, HashMap<String, HashMap<String, String>>> = HashMap::new();

    let hook_entries = match std::fs::read_dir(&hooks_dir) {
        Ok(entries) => entries,
        Err(_) => return result,
    };

    for hook_entry in hook_entries.flatten() {
        if !hook_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let hook_name = hook_entry.file_name().to_string_lossy().to_string();
        let step_entries = match std::fs::read_dir(hook_entry.path()) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut steps: HashMap<String, HashMap<String, String>> = HashMap::new();
        for step_entry in step_entries.flatten() {
            let file_name = step_entry.file_name().to_string_lossy().to_string();
            if let Some(step_name) = file_name.strip_suffix(".json") {
                if let Ok(contents) = std::fs::read_to_string(step_entry.path()) {
                    if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&contents) {
                        steps.insert(step_name.to_string(), map);
                    }
                }
            }
        }
        if !steps.is_empty() {
            result.insert(hook_name, steps);
        }
    }

    result
}

/// Write process outputs for a service.
pub fn write_process_outputs(
    config_dir: &Path,
    service: &str,
    outputs: &HashMap<String, String>,
) -> Result<()> {
    let dir = service_outputs_dir(config_dir, service);
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to create output dir {:?}: {}",
            dir, e
        ))
    })?;
    let path = dir.join("process.json");
    let json = serde_json::to_string(outputs).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to serialize process outputs: {}",
            e
        ))
    })?;
    std::fs::write(&path, json).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to write process outputs to {:?}: {}",
            path, e
        ))
    })?;
    Ok(())
}

/// Read process outputs for a service.
pub fn read_process_outputs(
    config_dir: &Path,
    service: &str,
) -> HashMap<String, String> {
    let path = service_outputs_dir(config_dir, service).join("process.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write final resolved outputs (process outputs merged with declared outputs).
pub fn write_resolved_outputs(
    config_dir: &Path,
    service: &str,
    outputs: &HashMap<String, String>,
) -> Result<()> {
    let dir = service_outputs_dir(config_dir, service);
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to create output dir {:?}: {}",
            dir, e
        ))
    })?;
    let path = dir.join("outputs.json");
    let json = serde_json::to_string(outputs).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to serialize resolved outputs: {}",
            e
        ))
    })?;
    std::fs::write(&path, json).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to write resolved outputs to {:?}: {}",
            path, e
        ))
    })?;
    Ok(())
}

/// Read combined outputs for dependent services.
///
/// Merges process outputs with resolved outputs (resolved takes precedence).
/// Falls back to just process outputs if no resolved outputs exist.
pub fn read_service_outputs(
    config_dir: &Path,
    service: &str,
) -> HashMap<String, String> {
    let dir = service_outputs_dir(config_dir, service);
    let outputs_path = dir.join("outputs.json");
    let process_path = dir.join("process.json");

    let mut result = HashMap::new();

    // Start with process outputs
    if let Ok(contents) = std::fs::read_to_string(&process_path) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&contents) {
            result.extend(map);
        }
    }

    // Override with resolved outputs (declarations take precedence)
    if let Ok(contents) = std::fs::read_to_string(&outputs_path) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&contents) {
            result.extend(map);
        }
    }

    result
}

/// Clear all outputs for a service.
pub fn clear_service_outputs(config_dir: &Path, service: &str) -> Result<()> {
    let dir = service_outputs_dir(config_dir, service);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| {
            crate::errors::DaemonError::Internal(format!(
                "Failed to clear output dir {:?}: {}",
                dir, e
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_capture_lines() {
        let lines = vec![
            "token=abc-123".to_string(),
            "host=localhost".to_string(),
            "port=8080".to_string(),
        ];
        let map = parse_capture_lines(&lines);
        assert_eq!(map.get("token"), Some(&"abc-123".to_string()));
        assert_eq!(map.get("host"), Some(&"localhost".to_string()));
        assert_eq!(map.get("port"), Some(&"8080".to_string()));
    }

    #[test]
    fn test_parse_capture_lines_last_wins() {
        let lines = vec![
            "key=first".to_string(),
            "key=second".to_string(),
        ];
        let map = parse_capture_lines(&lines);
        assert_eq!(map.get("key"), Some(&"second".to_string()));
    }

    #[test]
    fn test_parse_capture_lines_value_with_equals() {
        let lines = vec!["url=http://example.com?a=1&b=2".to_string()];
        let map = parse_capture_lines(&lines);
        assert_eq!(
            map.get("url"),
            Some(&"http://example.com?a=1&b=2".to_string())
        );
    }

    #[test]
    fn test_write_read_hook_step_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let outputs = HashMap::from([
            ("token".to_string(), "abc-123".to_string()),
            ("host".to_string(), "localhost".to_string()),
        ]);
        write_hook_step_outputs(dir.path(), "my-service", "pre_start", "step1", &outputs)
            .unwrap();
        let all = read_all_hook_outputs(dir.path(), "my-service");
        assert_eq!(
            all["pre_start"]["step1"]["token"],
            "abc-123"
        );
        assert_eq!(
            all["pre_start"]["step1"]["host"],
            "localhost"
        );
    }

    #[test]
    fn test_write_read_process_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let outputs = HashMap::from([
            ("result".to_string(), "hello".to_string()),
            ("port".to_string(), "8080".to_string()),
        ]);
        write_process_outputs(dir.path(), "runner", &outputs).unwrap();
        let read = read_process_outputs(dir.path(), "runner");
        assert_eq!(read["result"], "hello");
        assert_eq!(read["port"], "8080");
    }

    #[test]
    fn test_read_service_outputs_merges() {
        let dir = tempfile::tempdir().unwrap();
        let process = HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let resolved = HashMap::from([
            ("b".to_string(), "override".to_string()),
            ("c".to_string(), "3".to_string()),
        ]);
        write_process_outputs(dir.path(), "svc", &process).unwrap();
        write_resolved_outputs(dir.path(), "svc", &resolved).unwrap();

        let merged = read_service_outputs(dir.path(), "svc");
        assert_eq!(merged["a"], "1");
        assert_eq!(merged["b"], "override"); // resolved takes precedence
        assert_eq!(merged["c"], "3");
    }

    #[test]
    fn test_clear_service_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let outputs = HashMap::from([("k".to_string(), "v".to_string())]);
        write_process_outputs(dir.path(), "svc", &outputs).unwrap();
        assert!(!read_process_outputs(dir.path(), "svc").is_empty());

        clear_service_outputs(dir.path(), "svc").unwrap();
        assert!(read_process_outputs(dir.path(), "svc").is_empty());
    }

    #[test]
    fn test_parse_capture_lines_empty() {
        let lines: Vec<String> = vec![];
        let map = parse_capture_lines(&lines);
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_capture_lines_no_equals() {
        let lines = vec![
            "no_equals_here".to_string(),
            "another_line".to_string(),
        ];
        let map = parse_capture_lines(&lines);
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_capture_lines_empty_value() {
        let lines = vec!["key=".to_string()];
        let map = parse_capture_lines(&lines);
        assert_eq!(map.get("key"), Some(&"".to_string()));
    }

    #[test]
    fn test_parse_capture_lines_empty_key() {
        let lines = vec!["=value".to_string()];
        let map = parse_capture_lines(&lines);
        assert!(map.is_empty(), "Empty key should be rejected");
    }

    #[test]
    fn test_read_all_hook_outputs_multiple_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let step1_outputs = HashMap::from([
            ("token".to_string(), "abc".to_string()),
        ]);
        let cleanup_outputs = HashMap::from([
            ("cleaned".to_string(), "true".to_string()),
        ]);
        write_hook_step_outputs(dir.path(), "svc", "pre_start", "step1", &step1_outputs).unwrap();
        write_hook_step_outputs(dir.path(), "svc", "post_exit", "cleanup", &cleanup_outputs).unwrap();

        let all = read_all_hook_outputs(dir.path(), "svc");
        assert_eq!(all.len(), 2);
        assert_eq!(all["pre_start"]["step1"]["token"], "abc");
        assert_eq!(all["post_exit"]["cleanup"]["cleaned"], "true");
    }

    #[test]
    fn test_read_service_outputs_only_process() {
        let dir = tempfile::tempdir().unwrap();
        let process = HashMap::from([("a".to_string(), "1".to_string())]);
        write_process_outputs(dir.path(), "svc", &process).unwrap();

        let result = read_service_outputs(dir.path(), "svc");
        assert_eq!(result.len(), 1);
        assert_eq!(result["a"], "1");
    }

    #[test]
    fn test_read_service_outputs_only_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = HashMap::from([("b".to_string(), "2".to_string())]);
        write_resolved_outputs(dir.path(), "svc", &resolved).unwrap();

        let result = read_service_outputs(dir.path(), "svc");
        assert_eq!(result.len(), 1);
        assert_eq!(result["b"], "2");
    }

    #[test]
    fn test_read_service_outputs_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_service_outputs(dir.path(), "nonexistent");
        assert!(result.is_empty());
    }

    #[test]
    fn test_clear_nonexistent_service() {
        let dir = tempfile::tempdir().unwrap();
        let result = clear_service_outputs(dir.path(), "nonexistent");
        assert!(result.is_ok(), "Clearing outputs for nonexistent service should not error");
    }
}
