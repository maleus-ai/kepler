use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Cell, Row, Table};

use crate::ui::pages::top::app::TopApp;
use crate::ui::widgets::utils::format_bytes;

pub fn render(f: &mut Frame, area: Rect, app: &TopApp) {
    let theme = &app.theme;
    let has_toggles = !app.toggled_services.is_empty();

    // Compute column widths based on data
    let cpu_col_w: u16 = 8;   // e.g. "100.00%"
    let mem_col_w: u16 = 8;   // e.g. "123.4 MB"
    let status_col_w: u16 = 12; // e.g. "● running"
    let name_col_w = area.width.saturating_sub(2) // border
        .saturating_sub(cpu_col_w + mem_col_w + status_col_w + 1);

    // Pad "Name" header to align with highlight_symbol offset ("▌ " = 2 chars)
    let header = Row::new(vec![
        Cell::from(Span::styled("Name", theme.header_style())),
        Cell::from(Span::styled("CPU", theme.header_style())),
        Cell::from(Span::styled("Mem", theme.header_style())),
        Cell::from(Span::styled("Status", theme.header_style())),
    ]);

    let mut rows = Vec::new();

    // ALL row (index 0)
    let totals = app.compute_totals();
    let all_style = Style::default()
        .fg(theme.fg_dim)
        .add_modifier(Modifier::ITALIC);
    rows.push(Row::new(vec![
        Cell::from(Span::styled(format!("ALL ({})", app.services.len()), all_style)),
        Cell::from(Span::styled(
            format!("{:.2}%", totals.cpu_percent),
            all_style,
        )),
        Cell::from(Span::styled(
            format_bytes(totals.memory_rss),
            all_style,
        )),
        Cell::from(Span::styled("", all_style)),
    ]));

    // Individual services
    for svc in app.services.iter() {
        let is_toggled = app.is_toggled(svc);
        let chart_color = app.service_chart_color(svc);

        // Get status info
        let (status_str, status_icon, status_color) =
            if let Some(info) = app.service_status.get(svc) {
                let s = info.status.as_str();
                (s.to_string(), theme.status_icon(s), theme.status_color(s))
            } else {
                ("-".to_string(), "\u{25cb}", theme.fg_dim) // ○
            };

        // Prefix: focus indicator + toggle checkbox
        // Focus bar (▌) is shown via ratatui's row highlight (selected row)
        // Toggle checkbox is shown when multi-select is active
        let prefix = if has_toggles {
            if is_toggled {
                "\u{25c9} " // ◉
            } else {
                "\u{25cb} " // ○
            }
        } else {
            ""
        };

        // Truncate service name if needed
        let prefix_len = if has_toggles { 2u16 } else { 0u16 };
        let max_name_len = name_col_w.saturating_sub(prefix_len) as usize;
        let display_name = if svc.len() > max_name_len && max_name_len > 3 {
            format!("{}\u{2026}", &svc[..max_name_len - 1])
        } else {
            svc.clone()
        };

        let prefix_style = if has_toggles && is_toggled {
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.fg_dim)
        };

        let name_style = Style::default().fg(chart_color);

        // Name cell: prefix + name
        let name_cell = Cell::from(ratatui::text::Line::from(vec![
            Span::styled(prefix, prefix_style),
            Span::styled(display_name, name_style),
        ]));

        // CPU cell
        let cpu_cell = if let Some(metrics) = app.latest.get(svc) {
            Cell::from(Span::styled(
                format!("{:.2}%", metrics.cpu_percent),
                Style::default().fg(theme.fg_dim),
            ))
        } else {
            Cell::from(Span::styled("\u{2014}", Style::default().fg(theme.fg_dim)))
        };

        // Memory cell
        let mem_cell = if let Some(metrics) = app.latest.get(svc) {
            Cell::from(Span::styled(
                format_bytes(metrics.memory_rss),
                Style::default().fg(theme.fg_dim),
            ))
        } else {
            Cell::from(Span::styled("\u{2014}", Style::default().fg(theme.fg_dim)))
        };

        // Status cell
        let status_cell = Cell::from(Span::styled(
            format!("{} {}", status_icon, status_str),
            Style::default().fg(status_color),
        ));

        let row = Row::new(vec![name_cell, cpu_cell, mem_cell, status_cell]);

        // Dim if toggled off in multi-select mode
        let row = if has_toggles && !is_toggled {
            row.style(Style::default().fg(theme.fg_dim))
        } else {
            row
        };

        rows.push(row);
    }

    let title = " Services ";
    let block = theme.block_focused(title);

    let widths = [
        Constraint::Min(name_col_w),
        Constraint::Length(cpu_col_w),
        Constraint::Length(mem_col_w),
        Constraint::Length(status_col_w),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(theme.selection_style())
        .highlight_symbol("\u{258c}") // ▌ cursor bar
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);

    let mut state = ratatui::widgets::TableState::default()
        .with_selected(Some(app.selected))
        .with_offset(app.service_table_offset);
    f.render_stateful_widget(table, area, &mut state);
}
