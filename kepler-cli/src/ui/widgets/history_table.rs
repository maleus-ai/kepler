use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Cell, Row, Table};

use crate::ui::pages::top::app::TopApp;
use crate::ui::widgets::utils::format_bytes;

#[allow(dead_code)]
pub fn render(f: &mut Frame, area: Rect, app: &TopApp) {
    let theme = &app.theme;

    let visible = app.visible_services();
    let (title, entries) = if let Some(svc) = &app.focused_service {
        let title = format!(" History: {} ", svc);
        let entries = app.history.get(svc).cloned().unwrap_or_default();
        (title, entries)
    } else if !app.toggled_services.is_empty() {
        let count = app.toggled_services.len();
        let title = format!(" History: {} selected (Total) ", count);
        let entries = app.total_history.clone();
        (title, entries)
    } else if visible.len() == 1 {
        let svc = visible[0];
        let title = format!(" History: {} ", svc);
        let entries = app.history.get(svc).cloned().unwrap_or_default();
        (title, entries)
    } else {
        let title = " History: All (Total) ".to_string();
        let entries = app.total_history.clone();
        (title, entries)
    };

    let header = Row::new(vec![
        Cell::from("TIME"),
        Cell::from("CPU%"),
        Cell::from("MEM (RSS)"),
        Cell::from("MEM (VSS)"),
    ])
    .style(theme.header_style());

    let mut sorted: Vec<_> = entries.into_iter().collect();
    sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let visible: Vec<_> = sorted.iter().skip(app.history_scroll).collect();

    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let dt = chrono::DateTime::from_timestamp_millis(e.timestamp)
                .map(|t| t.with_timezone(&chrono::Local))
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "-".to_string());

            let style = if i % 2 == 0 {
                Style::default().fg(theme.fg)
            } else {
                Style::default().fg(theme.fg).bg(theme.bg_highlight)
            };

            Row::new(vec![
                Cell::from(dt),
                Cell::from(format!("{:.1}%", e.cpu_percent)),
                Cell::from(format_bytes(e.memory_rss)),
                Cell::from(format_bytes(e.memory_vss)),
            ])
            .style(style)
        })
        .collect();

    let block = theme.block(&title);

    let widths = [
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths).header(header).block(block);

    f.render_widget(table, area);
}
