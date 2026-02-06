//! Duration parsing and formatting utilities

use serde::{Deserialize, Deserializer, Serializer};
use std::time::Duration;

/// Parse duration string (e.g., "10s", "5m", "1h", "100ms")
pub fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty duration string".to_string());
    }

    // Find where the number ends and the unit begins
    let (num_str, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, "s")); // Default to seconds if no unit

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in duration: {}", num_str))?;

    let multiplier = match unit.to_lowercase().as_str() {
        "ms" => 1,
        "s" | "" => 1000,
        "m" => 60 * 1000,
        "h" => 60 * 60 * 1000,
        "d" => 24 * 60 * 60 * 1000,
        _ => return Err(format!("Unknown duration unit: {}", unit)),
    };

    let millis = num
        .checked_mul(multiplier)
        .ok_or_else(|| format!("Duration value too large: {}", s))?;
    Ok(Duration::from_millis(millis))
}

/// Format duration as string (e.g., "10s", "5m", "1h", "100ms")
pub fn format_duration(duration: &Duration) -> String {
    let millis = duration.as_millis() as u64;

    if millis == 0 {
        return "0s".to_string();
    }

    // Use the largest unit that divides evenly
    if millis.is_multiple_of(24 * 60 * 60 * 1000) {
        format!("{}d", millis / (24 * 60 * 60 * 1000))
    } else if millis.is_multiple_of(60 * 60 * 1000) {
        format!("{}h", millis / (60 * 60 * 1000))
    } else if millis.is_multiple_of(60 * 1000) {
        format!("{}m", millis / (60 * 1000))
    } else if millis.is_multiple_of(1000) {
        format!("{}s", millis / 1000)
    } else {
        format!("{}ms", millis)
    }
}

/// Deserialize duration from string like "10s", "5m", "1h"
pub fn deserialize_duration<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Serialize duration to string like "10s", "5m", "1h", "100ms"
pub fn serialize_duration<S>(duration: &Duration, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format_duration(duration))
}

/// Deserialize optional duration
pub fn deserialize_optional_duration<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// Serialize optional duration
pub fn serialize_optional_duration<S>(
    duration: &Option<Duration>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match duration {
        Some(d) => serializer.serialize_str(&format_duration(d)),
        None => serializer.serialize_none(),
    }
}
