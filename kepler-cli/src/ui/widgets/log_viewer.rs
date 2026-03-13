use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::pages::top::app::{ChartMode, TopApp};
use crate::ui::widgets::utils::format_timestamp_label;
pub fn render(f: &mut Frame, area: Rect, app: &mut TopApp) {
    let theme = &app.theme;

    let inner_height = area.height as usize;

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
    // Use all visible entries so alignment stays stable.
    let max_source_width = filtered
        .iter()
        .map(|e| match &e.hook {
            Some(h) => e.service.len() + 1 + h.len(), // "service.hook"
            None => e.service.len(),
        })
        .max()
        .unwrap_or(0);

    let is_following = matches!(app.chart_mode, ChartMode::Live) && app.log_viewer_cursor.is_none();

    let total = filtered.len();
    // Clamp log_scroll so it can't exceed scrollable range.
    // This prevents accumulating over-scroll that would require compensating
    // when scrolling back the other direction.
    let max_scroll = total.saturating_sub(inner_height);
    if app.log_scroll > max_scroll {
        app.log_scroll = max_scroll;
    }
    let scroll = if is_following { 0 } else { app.log_scroll };
    let skip = total.saturating_sub(inner_height + scroll);

    let visible: Vec<_> = filtered.iter().skip(skip).take(inner_height).collect();

    let cursor_id = app.log_viewer_cursor;

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let _abs_idx = skip + i;
            let is_cursor = cursor_id == Some(entry.id);

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
                "info" | "out" => "INFO",
                "warn" => "WARN",
                "error" | "err" => "ERROR",
                "fatal" => "FATAL",
                _ => "INFO",
            };

            let level_color = match entry.level.as_str() {
                "err" | "error" | "fatal" => theme.error,
                "warn" | "warning" => theme.warning,
                _ => theme.fg_dim,
            };

            // Build source column: "service" or "service.hook"
            let source = match &entry.hook {
                Some(h) => format!("{}.{}", entry.service, h),
                None => entry.service.clone(),
            };
            let padded_source = format!("{:<width$}", source, width = max_source_width);

            // Pad level to fixed width (5 chars covers "TRACE", "DEBUG", "ERROR", "FATAL")
            let padded_level = format!("{:<5}", display_level);

            let bg = if is_cursor { theme.bg_highlight } else { theme.bg_surface };

            Line::from(vec![
                Span::styled(
                    format!("[{}] ", ts),
                    Style::default().fg(theme.fg_dim).bg(bg),
                ),
                Span::styled(
                    padded_level,
                    Style::default().fg(level_color).bg(bg),
                ),
                Span::styled(
                    " ",
                    Style::default().bg(bg),
                ),
                Span::styled(
                    padded_source,
                    Style::default().fg(service_color).bg(bg),
                ),
                Span::styled(
                    " \u{2502} ", // │
                    Style::default().fg(theme.border).bg(bg),
                ),
                Span::styled(entry.line.clone(), Style::default().fg(text_color).bg(bg)),
            ])
        })
        .collect();

    // Show position indicator if not following
    let paragraph = if lines.is_empty() {
        let empty_msg = if app.services.is_empty() {
            "No services running"
        } else if !app.log_entries.is_empty() {
            "No logs in selected time range"
        } else {
            "No log entries yet. Logs will appear here when available."
        };
        Paragraph::new(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(theme.fg_dim),
        )))
        .style(Style::default().bg(theme.bg_surface))
    } else {
        Paragraph::new(lines).style(Style::default().bg(theme.bg_surface))
    };

    f.render_widget(paragraph, area);

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
