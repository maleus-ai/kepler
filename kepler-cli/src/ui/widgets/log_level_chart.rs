use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use ratatui::Frame;

use crate::ui::pages::top::app::TopApp;
use crate::ui::widgets::utils::{format_duration_label, format_timestamp_label};

/// Round up to the next value in the 1-2-5 scale:
/// 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, ...
fn nice_ceil(v: u32) -> u32 {
    if v <= 1 {
        return 1;
    }
    let mut magnitude = 1u32;
    while magnitude * 10 < v {
        magnitude *= 10;
    }
    for &m in &[1, 2, 5, 10] {
        let candidate = magnitude * m;
        if candidate >= v {
            return candidate;
        }
    }
    magnitude * 10
}

/// Log level categories: error=0, warn=1, info=2, debug=3, trace=4
fn level_category(level: &str) -> usize {
    match level {
        "err" | "error" | "fatal" => 0,
        "warn" | "warning" => 1,
        "info" | "out" => 2,
        "debug" => 3,
        "trace" => 4,
        _ => 2, // unknown → info
    }
}

const NUM_CATEGORIES: usize = 5;

fn format_count(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &TopApp, focused: bool) {
    let theme = &app.theme;
    let buf = f.buffer_mut();

    // Colors: error, warn, info, debug, trace
    let level_colors = [theme.error, theme.warning, theme.accent, theme.teal, theme.fg_dim];
    let level_names = ["error", "warn", "info", "debug", "trace"];

    // Compute time bounds
    let right_edge_ms = app.current_right_edge_ms();
    let window_ms = app.chart_window.as_millis() as i64;
    let left_edge_ms = right_edge_ms - window_ms;

    // Filter visible services
    let visible_svcs: Vec<&String> = app.visible_services();
    let show_all = app.is_all_view();

    // Count totals in the visible timeframe for the legend
    let mut totals = [0u32; NUM_CATEGORIES];
    for entry in &app.log_entries {
        if entry.timestamp < left_edge_ms || entry.timestamp > right_edge_ms {
            continue;
        }
        if !show_all && !visible_svcs.iter().any(|s| s.as_str() == entry.service) {
            continue;
        }
        let cat = level_category(&entry.level);
        totals[cat] += 1;
    }
    let grand_total: u32 = totals.iter().sum();

    // Build title with legend and counts
    let mut title_spans = vec![
        Span::styled(" Logs  ", Style::default().fg(theme.fg_dim)),
    ];
    for (i, &name) in level_names.iter().enumerate() {
        if totals[i] > 0 {
            title_spans.push(Span::styled("\u{25cf}", Style::default().fg(level_colors[i])));
            title_spans.push(Span::styled(
                format!(" {} {}  ", name, format_count(totals[i])),
                Style::default().fg(theme.fg_dim),
            ));
        }
    }
    // Total
    title_spans.push(Span::styled(
        format!("total {} ", format_count(grand_total)),
        Style::default().fg(theme.fg_dim),
    ));

    let title = Line::from(title_spans);

    let block = if focused {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border_focused))
            .title(title)
            .title_style(Style::default().fg(theme.fg_bright))
            .style(Style::default().bg(theme.bg_surface))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .title(title)
            .title_style(Style::default().fg(theme.fg_dim))
            .style(Style::default().bg(theme.bg_surface))
    };

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.width < 6 || inner.height < 3 {
        return;
    }

    // Reserve space: left 4 cols for y-axis labels, bottom 1 row for x-axis labels
    let y_label_width: u16 = 4;
    let x_label_height: u16 = 1;
    let data_area = Rect::new(
        inner.x + y_label_width,
        inner.y,
        inner.width.saturating_sub(y_label_width),
        inner.height.saturating_sub(x_label_height),
    );

    if data_area.width == 0 || data_area.height == 0 {
        return;
    }

    // Bucket log entries by column: counts per category per column
    let width = data_area.width as usize;
    let mut buckets: Vec<[u32; NUM_CATEGORIES]> = vec![[0; NUM_CATEGORIES]; width];

    let bucket_ms = window_ms / width as i64;
    let snapped_right = if bucket_ms > 0 {
        ((right_edge_ms + bucket_ms - 1) / bucket_ms) * bucket_ms
    } else {
        right_edge_ms
    };
    let snapped_left = snapped_right - bucket_ms * width as i64;

    for entry in &app.log_entries {
        if entry.timestamp < snapped_left || entry.timestamp >= snapped_right {
            continue;
        }
        if !show_all && !visible_svcs.iter().any(|s| s.as_str() == entry.service) {
            continue;
        }
        let col_idx = ((entry.timestamp - snapped_left) / bucket_ms) as usize;
        let col_idx = col_idx.min(width.saturating_sub(1));
        let cat = level_category(&entry.level);
        buckets[col_idx][cat] += 1;
    }

    // Find max total for y-axis scaling
    let max_total: u32 = buckets
        .iter()
        .map(|b| b.iter().sum::<u32>())
        .max()
        .unwrap_or(0);

    let max_y = nice_ceil(max_total);
    let data_height = data_area.height as u32;

    // Draw stacked bars: bottom-to-top order: error, warn, info, debug, trace
    for (col, counts) in buckets.iter().enumerate() {
        let total: u32 = counts.iter().sum();
        if total == 0 {
            continue;
        }

        let x = data_area.x + col as u16;

        // Compute heights for each category
        let mut heights = [0u16; NUM_CATEGORIES];
        for i in 0..NUM_CATEGORIES {
            heights[i] = (counts[i] as f64 / max_y as f64 * data_height as f64).round() as u16;
        }

        // Ensure non-empty buckets render at least 1 cell
        if heights.iter().sum::<u16>() == 0 {
            // Give 1 cell to the most prominent category (first non-zero)
            for i in 0..NUM_CATEGORIES {
                if counts[i] > 0 {
                    heights[i] = 1;
                    break;
                }
            }
        }

        // Draw from bottom upward: error(0), warn(1), info(2), debug(3), trace(4)
        let bottom_y = data_area.y + data_area.height;
        let mut y = bottom_y;

        for i in 0..NUM_CATEGORIES {
            for _ in 0..heights[i] {
                if y == 0 || y <= data_area.y {
                    break;
                }
                y -= 1;
                if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                    cell.set_char(' ');
                    cell.set_bg(level_colors[i]);
                }
            }
        }
    }

    // Draw y-axis labels
    let label_x = inner.x;
    let label_width = y_label_width as usize;

    let bottom_row = data_area.y + data_area.height - 1;
    write_label(buf, label_x, bottom_row, label_width, "0", theme.fg_dim);

    write_label(buf, label_x, data_area.y, label_width, &format!("{}", max_y), theme.fg_dim);

    if data_area.height > 2 {
        let mid_row = data_area.y + data_area.height / 2;
        let mid_val = max_y / 2;
        write_label(buf, label_x, mid_row, label_width, &format!("{}", mid_val), theme.fg_dim);
    }

    // Draw x-axis time labels
    let x_label_row = data_area.y + data_area.height;
    if x_label_row < inner.y + inner.height {
        let left_label = match app.chart_mode {
            crate::ui::pages::top::app::ChartMode::Live => {
                format!("-{}", format_duration_label(app.chart_window.as_secs()))
            }
            crate::ui::pages::top::app::ChartMode::Paused { right_edge_ms: edge } => {
                format_timestamp_label(edge - window_ms)
            }
        };
        let right_label = match app.chart_mode {
            crate::ui::pages::top::app::ChartMode::Live => "now".to_string(),
            crate::ui::pages::top::app::ChartMode::Paused { right_edge_ms: edge } => {
                format_timestamp_label(edge)
            }
        };

        write_str(buf, data_area.x, x_label_row, &left_label, theme.fg_dim);

        let right_x = (data_area.x + data_area.width).saturating_sub(right_label.len() as u16);
        write_str(buf, right_x, x_label_row, &right_label, theme.fg_dim);
    }
}

fn write_label(buf: &mut Buffer, x: u16, y: u16, max_width: usize, text: &str, fg: ratatui::style::Color) {
    let text = if text.len() > max_width {
        &text[..max_width]
    } else {
        text
    };
    let offset = max_width.saturating_sub(text.len());
    for (i, ch) in text.chars().enumerate() {
        let px = x + (offset + i) as u16;
        if let Some(cell) = buf.cell_mut(Position::new(px, y)) {
            cell.set_char(ch);
            cell.set_fg(fg);
        }
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, text: &str, fg: ratatui::style::Color) {
    for (i, ch) in text.chars().enumerate() {
        if let Some(cell) = buf.cell_mut(Position::new(x + i as u16, y)) {
            cell.set_char(ch);
            cell.set_fg(fg);
        }
    }
}
