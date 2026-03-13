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
/// e.g. 0→1, 3→5, 7→10, 47→50, 51→100, 120→200, 999→1000
fn nice_ceil(v: u32) -> u32 {
    if v <= 1 {
        return 1;
    }
    // Find the power of 10 that brackets v
    let mut magnitude = 1u32;
    while magnitude * 10 < v {
        magnitude *= 10;
    }
    // Try 1x, 2x, 5x, 10x of this magnitude
    for &m in &[1, 2, 5, 10] {
        let candidate = magnitude * m;
        if candidate >= v {
            return candidate;
        }
    }
    magnitude * 10
}

/// Classify a log level string into one of three categories.
fn level_category(level: &str) -> usize {
    match level {
        "err" | "error" | "fatal" => 0, // error
        "warn" | "warning" => 1,        // warn
        _ => 2,                          // info (includes "out", "info", "debug")
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &TopApp, focused: bool) {
    let theme = &app.theme;
    let buf = f.buffer_mut();

    let level_colors = [theme.error, theme.warning, theme.accent];

    // Build block with legend in title
    let title = Line::from(vec![
        Span::styled(" Log Levels  ", Style::default().fg(theme.fg_dim)),
        Span::styled("\u{25cf}", Style::default().fg(level_colors[0])),
        Span::styled(" err  ", Style::default().fg(theme.fg_dim)),
        Span::styled("\u{25cf}", Style::default().fg(level_colors[1])),
        Span::styled(" warn  ", Style::default().fg(theme.fg_dim)),
        Span::styled("\u{25cf}", Style::default().fg(level_colors[2])),
        Span::styled(" info ", Style::default().fg(theme.fg_dim)),
    ]);

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

    // Compute time bounds with snapped bucket boundaries.
    // Without snapping, the sliding window causes log entries to drift between
    // adjacent columns on each frame, making bar heights fluctuate visually.
    let right_edge_ms = app.current_right_edge_ms();
    let window_ms = app.chart_window.as_millis() as i64;

    // Filter visible services
    let visible_svcs: Vec<&String> = app.visible_services();
    let show_all = app.is_all_view();

    // Bucket log entries by column: (err_count, warn_count, info_count) per column
    let width = data_area.width as usize;
    let mut buckets: Vec<(u32, u32, u32)> = vec![(0, 0, 0); width];

    // Each bucket covers a fixed time interval. Snap the right edge so bucket
    // boundaries align to multiples of bucket_ms, preventing entries from
    // migrating between columns as the window slides.
    let bucket_ms = window_ms / width as i64;
    let snapped_right = if bucket_ms > 0 {
        // Round up to the next bucket boundary
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
        match cat {
            0 => buckets[col_idx].0 += 1,
            1 => buckets[col_idx].1 += 1,
            _ => buckets[col_idx].2 += 1,
        }
    }

    // Find max total for y-axis scaling
    let max_total: u32 = buckets
        .iter()
        .map(|(e, w, i)| e + w + i)
        .max()
        .unwrap_or(0);

    // Round up to the next "nice" ceiling from the scale:
    // 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, ...
    // This keeps the y-axis stable — e.g. 47 rounds to 50, 51 rounds to 100,
    // so the scale only changes at wide intervals.
    let max_y = nice_ceil(max_total);

    let data_height = data_area.height as u32;

    // Draw stacked bars
    for (col, &(err, warn, info)) in buckets.iter().enumerate() {
        let total = err + warn + info;
        if total == 0 {
            continue;
        }

        let x = data_area.x + col as u16;

        // Heights scaled proportionally, bottom-to-top: error, warn, info
        // Ensure non-empty buckets render at least 1 cell total so they remain
        // visually distinguishable from truly empty buckets.
        let mut err_h = (err as f64 / max_y as f64 * data_height as f64).round() as u16;
        let mut warn_h = (warn as f64 / max_y as f64 * data_height as f64).round() as u16;
        let mut info_h = (info as f64 / max_y as f64 * data_height as f64).round() as u16;
        if err_h + warn_h + info_h == 0 {
            // Give 1 cell to the most prominent category
            if err > 0 { err_h = 1; }
            else if warn > 0 { warn_h = 1; }
            else { info_h = 1; }
        }

        // Draw from bottom of data_area upward
        let bottom_y = data_area.y + data_area.height;
        let mut y = bottom_y;

        // Error (red) at bottom
        for _ in 0..err_h {
            if y == 0 || y <= data_area.y {
                break;
            }
            y -= 1;
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_char(' ');
                cell.set_bg(level_colors[0]);
            }
        }
        // Warn (yellow) in middle
        for _ in 0..warn_h {
            if y == 0 || y <= data_area.y {
                break;
            }
            y -= 1;
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_char(' ');
                cell.set_bg(level_colors[1]);
            }
        }
        // Info (blue) on top
        for _ in 0..info_h {
            if y == 0 || y <= data_area.y {
                break;
            }
            y -= 1;
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_char(' ');
                cell.set_bg(level_colors[2]);
            }
        }
    }

    // Draw y-axis labels (left of data area): 0 at bottom, max at top, mid in middle
    let label_x = inner.x;
    let label_width = y_label_width as usize;

    // Bottom label (0)
    let bottom_row = data_area.y + data_area.height - 1;
    write_label(buf, label_x, bottom_row, label_width, "0", theme.fg_dim);

    // Top label (max)
    write_label(buf, label_x, data_area.y, label_width, &format!("{}", max_y), theme.fg_dim);

    // Mid label
    if data_area.height > 2 {
        let mid_row = data_area.y + data_area.height / 2;
        let mid_val = max_y / 2;
        write_label(buf, label_x, mid_row, label_width, &format!("{}", mid_val), theme.fg_dim);
    }

    // Draw x-axis time labels (below data area)
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

        // Left label
        write_str(buf, data_area.x, x_label_row, &left_label, theme.fg_dim);

        // Right label (right-aligned)
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
    // Right-align within label area
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
