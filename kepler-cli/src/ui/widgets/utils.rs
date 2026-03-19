use std::collections::VecDeque;
use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::ui::theme::Theme;
use kepler_protocol::protocol::MonitorMetricEntry;

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Round up to the next value in the 1-2-5 scale (float version).
/// Returns at least `min_value`.
pub fn nice_ceil_f64(v: f64, min_value: f64) -> f64 {
    let v = v.max(min_value);
    if v <= 0.0 {
        return min_value;
    }
    let exp = v.log10().floor();
    let magnitude = 10.0_f64.powf(exp);
    let normalized = v / magnitude;
    let nice = if normalized <= 1.0 { 1.0 }
        else if normalized <= 2.0 { 2.0 }
        else if normalized <= 5.0 { 5.0 }
        else { 10.0 };
    (nice * magnitude).max(min_value)
}

/// Round bytes up to a nice binary-friendly value (1, 2, 4, 8, 16, ... × unit).
/// Uses powers-of-two multipliers within each binary unit (KB, MB, GB).
pub fn nice_ceil_bytes(v: f64, min_value: f64) -> f64 {
    let v = v.max(min_value);
    if v <= 0.0 {
        return min_value;
    }
    // Find the binary unit (KB=1024, MB=1024², GB=1024³, ...)
    let units: &[f64] = &[
        1024.0,                         // KB
        1024.0 * 1024.0,                // MB
        1024.0 * 1024.0 * 1024.0,       // GB
        1024.0 * 1024.0 * 1024.0 * 1024.0, // TB
    ];
    let unit = units.iter().rev().find(|&&u| v >= u).copied().unwrap_or(1.0);
    let normalized = v / unit;
    // Round up to next nice multiplier: 1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024
    let nice = (2.0_f64).powf(normalized.log2().ceil());
    (nice * unit).max(min_value)
}

pub fn service_color(index: usize, theme: &Theme) -> Color {
    theme.chart_colors[index % theme.chart_colors.len()]
}

/// Convert history entries to chart data points (seconds_ago, value).
/// `right_edge_ms` is the timestamp at the right edge of the chart (now for live, frozen for paused).
pub fn chart_data_points(
    history: &VecDeque<MonitorMetricEntry>,
    window: Duration,
    right_edge_ms: i64,
    extractor: fn(&MonitorMetricEntry) -> f64,
) -> Vec<(f64, f64)> {
    let window_ms = window.as_millis() as i64;
    let cutoff = right_edge_ms - window_ms;

    history
        .iter()
        .filter(|e| e.timestamp >= cutoff && e.timestamp <= right_edge_ms)
        .map(|e| {
            let seconds_ago = (right_edge_ms - e.timestamp) as f64 / 1000.0;
            // X axis: 0 = window_start, window_secs = right_edge
            let x = (window.as_secs_f64()) - seconds_ago;
            (x, extractor(e))
        })
        .collect()
}

/// Estimate the inner data area of a ratatui Chart widget.
/// Subtracts borders (1 each side), y-axis labels, and x-axis row.
pub fn estimate_chart_data_area(chart_area: Rect, y_label_width: u16) -> Rect {
    // Left: 1 border + y_label_width + 1 gap
    let left_offset = 1 + y_label_width + 1;
    // Right: 1 border
    let right_offset = 1;
    // Top: 1 border + 1 title
    let top_offset = 2;
    // Bottom: 1 x-axis labels + 1 border
    let bottom_offset = 2;

    let x = chart_area.x + left_offset;
    let y = chart_area.y + top_offset;
    let width = chart_area.width.saturating_sub(left_offset + right_offset);
    let height = chart_area.height.saturating_sub(top_offset + bottom_offset);
    Rect::new(x, y, width, height)
}

/// Map a terminal column to the data X coordinate.
pub fn pixel_to_data_x(col: u16, data_area: Rect, x_bounds: [f64; 2]) -> Option<f64> {
    if col < data_area.x || col >= data_area.x + data_area.width {
        return None;
    }
    let relative = (col - data_area.x) as f64;
    let fraction = relative / data_area.width as f64;
    let x = x_bounds[0] + fraction * (x_bounds[1] - x_bounds[0]);
    Some(x)
}

/// Find nearest interpolated values for each dataset at the given query_x.
pub fn find_nearest_values(
    datasets: &[(String, Vec<(f64, f64)>, Color)],
    query_x: f64,
) -> Vec<(String, f64, Color)> {
    let mut results = Vec::new();
    for (name, points, color) in datasets {
        if points.is_empty() {
            continue;
        }
        // Find bracketing points for linear interpolation
        let mut before: Option<&(f64, f64)> = None;
        let mut after: Option<&(f64, f64)> = None;
        for p in points {
            if p.0 <= query_x {
                before = Some(p);
            }
            if p.0 >= query_x && after.is_none() {
                after = Some(p);
            }
        }
        let value = match (before, after) {
            (Some(b), Some(a)) if (a.0 - b.0).abs() > f64::EPSILON => {
                // Linear interpolation
                let t = (query_x - b.0) / (a.0 - b.0);
                b.1 + t * (a.1 - b.1)
            }
            (Some(b), _) => b.1,
            (_, Some(a)) => a.1,
            _ => continue,
        };
        results.push((name.clone(), value, *color));
    }
    results
}

/// Format a timestamp in milliseconds as HH:MM:SS using local time.
pub fn format_timestamp_label(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    if let Some(dt) = Local.timestamp_millis_opt(ms).single() {
        dt.format("%H:%M:%S").to_string()
    } else {
        "??:??:??".to_string()
    }
}

/// Format a duration as a human-readable string for chart labels.
/// e.g. 30 → "30s", 300 → "5m", 3600 → "1h", 86400 → "1d"
pub fn format_duration_label(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{}m", m)
        } else {
            format!("{}m{}s", m, s)
        }
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h{}m", h, m)
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d{}h", d, h)
        }
    }
}

/// Word-wrap a single line of text to fit within `width` characters.
/// Breaks at spaces; words longer than `width` are hard-broken by char.
/// Returns a list of wrapped lines (never empty — at least one empty string).
pub fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;

    for word in text.split(' ') {
        let word_len = word.chars().count();

        if word_len > width {
            // Hard-break long word
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(width) {
                let chunk_len = chunk.len();
                if current_len + chunk_len + if current_len > 0 { 1 } else { 0 } > width && current_len > 0 {
                    lines.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                if current_len > 0 {
                    current.push(' ');
                    current_len += 1;
                }
                let s: String = chunk.iter().collect();
                current.push_str(&s);
                current_len += chunk_len;
            }
        } else if current_len == 0 {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        }
    }

    lines.push(current);
    lines
}

/// Word-wrap text that may contain newlines.
/// Splits on '\n' first, then word-wraps each sub-line.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    for sub in text.split('\n') {
        result.extend(word_wrap(sub, width));
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// Count how many visual lines `text` would occupy when word-wrapped at `width`.
pub fn wrap_text_line_count(text: &str, width: usize) -> usize {
    wrap_text(text, width).len().max(1)
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_width = area.width * percent_x / 100;
    let popup_height = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}
