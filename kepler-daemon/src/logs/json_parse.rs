//! JSON log line parsing.
//!
//! Extracts message, level, and timestamp from structured JSON log lines
//! (pino, bunyan, winston, log4j, Python logging, Go slog, etc.).
//! Remaining fields are returned as a JSON attributes string.

use super::normalize_level;

/// Result of parsing a JSON log line.
pub(crate) struct ParsedJsonLog {
    /// Extracted message (from `msg` or `message` field)
    pub message: String,
    /// Normalized level string (e.g. "info", "warn", "error")
    pub level: Option<&'static str>,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: Option<i64>,
    /// Remaining JSON fields serialized as a JSON string, or None if empty
    pub attributes: Option<String>,
}

/// Known message field names (checked in order of priority).
const MSG_FIELDS: &[&str] = &[
    "msg",       // pino, bunyan, Go slog
    "message",   // winston, datadog, ELK, python logging, log4j
    "body",      // OpenTelemetry
    "text",      // some custom loggers
    "log",       // Docker JSON log driver, Fluent Bit
    "err",       // error-only loggers
    "error",     // error-only loggers
];

/// Known level field names.
const LEVEL_FIELDS: &[&str] = &[
    "level",     // pino, bunyan, winston, log4j
    "severity",  // GCP Cloud Logging, OpenTelemetry
    "log_level", // Datadog
    "loglevel",  // alternate
    "levelname", // Python logging
    "log.level", // ECS (Elastic Common Schema)
    "priority",  // syslog-style
];

/// Known timestamp field names.
const TIME_FIELDS: &[&str] = &[
    "time",       // pino, bunyan
    "timestamp",  // winston, generic
    "ts",         // Go zap
    "datetime",   // Python logging
    "date",       // generic
    "@timestamp", // ECS (Elastic Common Schema), Logstash
    "logged_at",  // some Ruby loggers
];

/// Try to parse a log line as structured JSON.
///
/// Returns `None` if the line is not valid JSON or not a JSON object.
pub(crate) fn try_parse_json_log(line: &str) -> Option<ParsedJsonLog> {
    let mut obj: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(line).ok()?;

    // Extract message
    let message = extract_string_field(&mut obj, MSG_FIELDS);

    // Extract level
    let level = extract_level(&mut obj);

    // Extract timestamp
    let timestamp = extract_timestamp(&mut obj);

    // Remaining fields → attributes
    let attributes = if obj.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&obj).unwrap_or_default())
    };

    Some(ParsedJsonLog {
        message: message.unwrap_or_else(|| line.to_string()),
        level,
        timestamp,
        attributes,
    })
}

/// Extract and remove the first matching string field from the object.
fn extract_string_field(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    field_names: &[&str],
) -> Option<String> {
    for &name in field_names {
        if let Some(val) = obj.remove(name) {
            return match val {
                serde_json::Value::String(s) => Some(s),
                other => Some(other.to_string()),
            };
        }
    }
    None
}

/// Extract and normalize the level field.
///
/// Handles string levels (case-insensitive) directly.
/// Numeric levels are left in attributes (ambiguous between conventions).
fn extract_level(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<&'static str> {
    for &name in LEVEL_FIELDS {
        if let Some(val) = obj.get(name) {
            match val {
                serde_json::Value::String(s) => {
                    let normalized = normalize_level(s);
                    obj.remove(name);
                    return Some(normalized);
                }
                // Numeric levels are ambiguous — leave them in attributes
                _ => return None,
            }
        }
    }
    None
}

/// Extract and parse the timestamp field.
///
/// Supports:
/// - Integer milliseconds since epoch (> 1_000_000_000_000)
/// - Integer seconds since epoch (> 1_000_000_000), converted to millis
/// - Float seconds since epoch, converted to millis
/// - ISO 8601 strings
fn extract_timestamp(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<i64> {
    for &name in TIME_FIELDS {
        if let Some(val) = obj.get(name) {
            let ts = match val {
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        if i > 1_000_000_000_000 {
                            Some(i) // already milliseconds
                        } else if i > 1_000_000_000 {
                            Some(i * 1000) // seconds → millis
                        } else {
                            None // too small to be a unix timestamp
                        }
                    } else if let Some(f) = n.as_f64() {
                        if f > 1_000_000_000.0 {
                            Some((f * 1000.0) as i64)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                serde_json::Value::String(s) => {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                        .or_else(|| {
                            // Try ISO 8601 without timezone (assume UTC)
                            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                                .ok()
                                .map(|dt| dt.and_utc().timestamp_millis())
                        })
                }
                _ => None,
            };
            if ts.is_some() {
                obj.remove(name);
                return ts;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pino_format() {
        let line = r#"{"level":"info","time":1531171074631,"msg":"hello world","pid":657,"hostname":"myhost"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.message, "hello world");
        assert_eq!(parsed.level, Some("info"));
        assert_eq!(parsed.timestamp, Some(1531171074631));
        // pid and hostname remain in attributes
        let attrs: serde_json::Value = serde_json::from_str(parsed.attributes.as_ref().unwrap()).unwrap();
        assert_eq!(attrs["pid"], 657);
        assert_eq!(attrs["hostname"], "myhost");
    }

    #[test]
    fn test_pino_numeric_level_stays_in_attributes() {
        let line = r#"{"level":30,"time":1531171074631,"msg":"hello"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.message, "hello");
        assert_eq!(parsed.level, None); // numeric → ambiguous, not extracted
        assert_eq!(parsed.timestamp, Some(1531171074631));
        let attrs: serde_json::Value = serde_json::from_str(parsed.attributes.as_ref().unwrap()).unwrap();
        assert_eq!(attrs["level"], 30);
    }

    #[test]
    fn test_winston_format() {
        let line = r#"{"level":"warn","message":"disk space low","timestamp":"2024-01-15T10:30:00.000Z"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.message, "disk space low");
        assert_eq!(parsed.level, Some("warn"));
        assert!(parsed.timestamp.is_some());
        assert!(parsed.attributes.is_none()); // all fields extracted
    }

    #[test]
    fn test_level_normalization() {
        let line = r#"{"level":"WARNING","msg":"caution"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.level, Some("warn"));

        let line = r#"{"level":"CRITICAL","msg":"panic"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.level, Some("fatal"));
    }

    #[test]
    fn test_timestamp_seconds() {
        let line = r#"{"msg":"hi","ts":1700000000}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.timestamp, Some(1700000000000));
    }

    #[test]
    fn test_not_json() {
        assert!(try_parse_json_log("hello world").is_none());
        assert!(try_parse_json_log("[1, 2, 3]").is_none()); // array, not object
    }

    #[test]
    fn test_empty_object() {
        let parsed = try_parse_json_log("{}").unwrap();
        assert_eq!(parsed.message, "{}");
        assert_eq!(parsed.level, None);
        assert_eq!(parsed.timestamp, None);
        assert!(parsed.attributes.is_none());
    }

    #[test]
    fn test_msg_not_string() {
        let line = r#"{"msg":42,"level":"debug"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.message, "42");
        assert_eq!(parsed.level, Some("debug"));
    }

    #[test]
    fn test_severity_field() {
        let line = r#"{"severity":"error","msg":"fail"}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.level, Some("error"));
    }

    #[test]
    fn test_no_message_field_preserves_original_line() {
        let line = r#"{"level":"info","status":"ok","count":42}"#;
        let parsed = try_parse_json_log(line).unwrap();
        assert_eq!(parsed.message, line);
        assert_eq!(parsed.level, Some("info"));
        // status and count remain in attributes
        let attrs: serde_json::Value = serde_json::from_str(parsed.attributes.as_ref().unwrap()).unwrap();
        assert_eq!(attrs["status"], "ok");
        assert_eq!(attrs["count"], 42);
    }
}
