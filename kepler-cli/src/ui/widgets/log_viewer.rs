use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::ui::pages::top::app::{ChartMode, TopApp};
use crate::ui::widgets::utils::{format_timestamp_label, wrap_text, wrap_text_line_count};

pub fn render(f: &mut Frame, area: Rect, app: &mut TopApp) {
    let theme = &app.theme;

    let inner_height = area.height as usize;
    let inner_width = area.width as usize;

    // Filter log entries based on time window and visible services
    let right_edge_ms = app.current_right_edge_ms();
    let window_ms = app.chart_window.as_millis() as i64;
    let left_edge_ms = right_edge_ms - window_ms;

    let visible_svcs: Vec<&String> = app.visible_services();
    let show_all = app.is_all_view();
    let filtered: Vec<_> = app
        .log_entries
        .iter()
        .filter(|e| e.timestamp >= left_edge_ms && e.timestamp <= right_edge_ms)
        .filter(|e| show_all || visible_svcs.iter().any(|s| s.as_str() == e.service))
        .collect();

    // Compute max service/hook name width for column alignment.
    let max_source_width = filtered
        .iter()
        .map(|e| match &e.hook {
            Some(h) => e.service.len() + 1 + h.len(),
            None => e.service.len(),
        })
        .max()
        .unwrap_or(0);

    let is_following = matches!(app.chart_mode, ChartMode::Live) && app.log_viewer_cursor.is_none();

    let total = filtered.len();

    // Column widths: [HH:MM:SS] + spacing(1) + LEVEL + spacing(1) + source + spacing(1) + │ + spacing(1) + message
    let ts_col_w: u16 = 10; // "[HH:MM:SS]"
    let level_col_w: u16 = 5; // "LEVEL"
    let source_col_w: u16 = max_source_width as u16;
    let sep_col_w: u16 = 1; // "│"
    let col_spacing: u16 = 1;
    let prefix_total = ts_col_w + level_col_w + source_col_w + sep_col_w + col_spacing * 4;
    let msg_col_w = (inner_width as u16).saturating_sub(prefix_total) as usize;

    // Compute visual line count per filtered entry (handles \n and word wrapping)
    let entry_vlines: Vec<usize> = filtered
        .iter()
        .map(|e| wrap_text_line_count(&e.line, msg_col_w))
        .collect();

    // Compute max_scroll: entry-based, but adjusted for wrapping.
    // Walk from top to find how many entries fill one screenful.
    let fits_from_top = if total == 0 {
        0
    } else {
        let mut vl = 0usize;
        let mut count = 0usize;
        for &evl in &entry_vlines {
            if vl + evl > inner_height && count > 0 {
                break;
            }
            vl += evl;
            count += 1;
            if vl >= inner_height {
                break;
            }
        }
        count
    };
    let max_scroll = total.saturating_sub(fits_from_top.max(1));

    if app.log_scroll > max_scroll {
        app.log_scroll = max_scroll;
    }
    let scroll = if is_following { 0 } else { app.log_scroll };

    // Select visible entries: walk backward from the end, skip `scroll` entries,
    // then walk backward to fill `inner_height` visual lines.
    let bottom_idx = if total == 0 {
        0
    } else {
        total.saturating_sub(1 + scroll)
    };

    let (visible_start, visible_end) = if total == 0 {
        (0, 0)
    } else {
        let mut start = bottom_idx;
        let mut vl = entry_vlines[bottom_idx];
        while start > 0 && vl < inner_height {
            start -= 1;
            vl += entry_vlines[start];
        }
        // If the first entry's lines would push us over, keep it (partial clip is OK)
        (start, bottom_idx + 1)
    };

    let visible: Vec<_> = filtered[visible_start..visible_end].iter().collect();
    let visible_vlines: Vec<usize> = entry_vlines[visible_start..visible_end].to_vec();

    let cursor_id = app.log_viewer_cursor;

    // Build row-to-entry-id mapping for click detection
    app.log_viewer_row_entry_ids.clear();

    let mut rows: Vec<Row> = Vec::new();
    for (vi, entry) in visible.iter().enumerate() {
        let is_cursor = cursor_id == Some(entry.id);
        let vlines = visible_vlines[vi];

        let ts = format_timestamp_label(entry.timestamp);
        let service_color = app.service_chart_color(&entry.service);
        let text_color = if entry.level == "err" {
            theme.peach
        } else {
            theme.fg
        };

        let display_level = match entry.level.as_str() {
            "trace" => "TRACE",
            "debug" => "DEBUG",
            "info" => "INFO",
            "warn" => "WARN",
            "error" => "ERROR",
            "fatal" => "FATAL",
            _ => "INFO",
        };

        let level_color = match entry.level.as_str() {
            "error" | "fatal" => theme.error,
            "warn" | "warning" => theme.warning,
            "info" => theme.accent,
            "debug" => theme.teal,
            "trace" => theme.fg_dim,
            _ => theme.fg_dim,
        };

        let source = match &entry.hook {
            Some(h) => format!("{}.{}", entry.service, h),
            None => entry.service.clone(),
        };
        let padded_source = format!("{:<width$}", source, width = max_source_width);
        let padded_level = format!("{:<5}", display_level);

        let bg = if is_cursor {
            theme.bg_highlight
        } else {
            theme.bg_surface
        };

        // Timestamp cell: content on first line only
        let ts_cell = Cell::from(Span::styled(
            format!("[{}]", ts),
            Style::default().fg(theme.fg_dim),
        ));

        // Level cell: content on first line only
        let level_cell = Cell::from(Span::styled(
            padded_level,
            Style::default().fg(level_color),
        ));

        // Source cell: content on first line only
        let source_cell = Cell::from(Span::styled(
            padded_source,
            Style::default().fg(service_color),
        ));

        // Separator cell: │ on every line
        let sep_lines: Vec<Line> = (0..vlines)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(theme.border))))
            .collect();
        let sep_cell = Cell::from(Text::from(sep_lines));

        // Message cell: word-wrap (handles \n and long words)
        let msg_lines: Vec<Line> = wrap_text(&entry.line, msg_col_w)
            .into_iter()
            .map(|chunk| Line::from(Span::styled(chunk, Style::default().fg(text_color))))
            .collect();
        let msg_cell = Cell::from(Text::from(msg_lines));

        let row = Row::new(vec![ts_cell, level_cell, source_cell, sep_cell, msg_cell])
            .height(vlines as u16)
            .style(Style::default().bg(bg));

        rows.push(row);

        // Map each visual row of this entry to its ID
        for _ in 0..vlines {
            app.log_viewer_row_entry_ids.push(entry.id);
        }
    }

    if rows.is_empty() {
        let empty_msg = if app.services.is_empty() {
            "No services running"
        } else if !app.log_entries.is_empty() {
            "No logs in selected time range"
        } else {
            "No log entries yet. Logs will appear here when available."
        };
        let paragraph = Paragraph::new(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(theme.fg_dim),
        )))
        .style(Style::default().bg(theme.bg_surface));
        f.render_widget(paragraph, area);
    } else {
        let widths = [
            Constraint::Length(ts_col_w),
            Constraint::Length(level_col_w),
            Constraint::Length(source_col_w),
            Constraint::Length(sep_col_w),
            Constraint::Min(0),
        ];

        let table = Table::new(rows, widths)
            .column_spacing(1)
            .style(Style::default().bg(theme.bg_surface));

        f.render_widget(table, area);
    }

    // Render scroll position indicator in bottom-right of log area
    if !is_following && total > inner_height {
        let pos_text = format!(
            " [{}/{}] ",
            total.saturating_sub(app.log_scroll).min(total),
            total
        );
        let pos_width = pos_text.len() as u16;
        let pos_x = (area.x + area.width).saturating_sub(pos_width + 1);
        let pos_y = area.y + area.height - 1;
        if pos_x > area.x && pos_y > area.y {
            let pos_rect = Rect::new(pos_x, pos_y, pos_width, 1);
            let pos_paragraph = Paragraph::new(Span::styled(
                pos_text,
                Style::default().fg(theme.fg_dim).bg(theme.bg_surface),
            ));
            f.render_widget(pos_paragraph, pos_rect);
        }
    }
}
