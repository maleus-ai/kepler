use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::ui::pages::top::app::{LogFocus, TopApp};
use crate::ui::widgets::utils::format_timestamp_label;

pub fn render(f: &mut Frame, area: Rect, app: &mut TopApp) {
    let theme = &app.theme;

    let focused = app.log_focus == LogFocus::Detail;
    let border_color = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(vec![
            Span::styled(" Log Detail ", Style::default().fg(theme.accent)),
        ]))
        .title_alignment(Alignment::Left)
        .style(Style::default().bg(theme.bg_surface));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let entry = match &app.log_detail_entry {
        Some(e) => e.clone(),
        None => {
            let msg = Paragraph::new("No log entry selected")
                .style(Style::default().fg(theme.fg_dim));
            f.render_widget(msg, inner);
            return;
        }
    };

    let selected = app.log_detail_selected;
    let section_style = Style::default()
        .fg(theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(theme.fg_dim);
    let value_style = Style::default().fg(theme.fg);
    let highlight_bg = theme.bg_highlight;
    let inner_w = inner.width.saturating_sub(2) as usize; // padding

    let mut lines: Vec<Line> = Vec::new();
    // Track (line_start, line_count, copyable_index) for click area computation
    let mut click_ranges: Vec<(usize, usize, usize)> = Vec::new();

    // ── Shortcuts bar ───────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::styled(" [v] ", Style::default().fg(theme.accent)),
        Span::styled("View in context", label_style),
        Span::styled("  ", label_style),
        Span::styled("[c] ", Style::default().fg(theme.accent)),
        Span::styled("Copy selected", label_style),
        Span::styled("  ", label_style),
        Span::styled("[Esc] ", Style::default().fg(theme.accent)),
        Span::styled("Close", label_style),
    ]));
    lines.push(Line::from(""));

    // ── Metadata ────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(" Metadata", section_style)));
    lines.push(Line::from(""));

    let ts_str = format_timestamp_label(entry.timestamp);
    lines.push(Line::from(vec![
        Span::styled("  Timestamp  ", label_style),
        Span::styled(ts_str, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Service    ", label_style),
        Span::styled(&entry.service, Style::default().fg(theme.accent)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Level      ", label_style),
        Span::styled(&entry.level, value_style),
    ]));
    if let Some(ref hook) = entry.hook {
        lines.push(Line::from(vec![
            Span::styled("  Hook       ", label_style),
            Span::styled(hook, value_style),
        ]));
    }
    lines.push(Line::from(""));

    // ── Log ───────────────────────────────────────────────────────
    let raw_selected = selected == 0;
    let raw_bg = if raw_selected { highlight_bg } else { theme.bg_surface };

    lines.push(Line::from(vec![
        Span::styled(" Log", section_style),
        Span::styled(
            if raw_selected { "  ← Tab to cycle, c to copy" } else { "" },
            Style::default().fg(theme.fg_dim),
        ),
    ]));
    lines.push(Line::from(""));

    // Word-wrap the raw text — track lines for click area
    let raw_line_start = lines.len();
    if inner_w > 2 {
        let wrap_w = inner_w - 2;
        let text = &entry.line;
        let mut start = 0;
        while start < text.len() {
            let end = (start + wrap_w).min(text.len());
            lines.push(Line::from(Span::styled(
                format!("  {}", &text[start..end]),
                Style::default().fg(theme.fg).bg(raw_bg),
            )));
            start = end;
        }
        if text.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (empty)",
                Style::default().fg(theme.fg_dim).bg(raw_bg),
            )));
        }
    }
    let raw_line_count = lines.len() - raw_line_start;
    click_ranges.push((raw_line_start, raw_line_count, 0));
    lines.push(Line::from(""));

    // ── Attributes ──────────────────────────────────────────────────
    let mut copyable_count: usize = 1; // raw text is always index 0

    if let Some(ref attrs_str) = entry.attributes {
        lines.push(Line::from(Span::styled(" Attributes", section_style)));
        lines.push(Line::from(""));

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(attrs_str) {
            if let Some(obj) = parsed.as_object() {
                for (i, (key, value)) in obj.iter().enumerate() {
                    let attr_idx = i + 1; // 1-based (0 is raw text)
                    let is_selected = selected == attr_idx;
                    let bg = if is_selected { highlight_bg } else { theme.bg_surface };
                    copyable_count += 1;

                    let val_str = match value.as_str() {
                        Some(s) => s.to_string(),
                        None => value.to_string(),
                    };

                    let attr_line_idx = lines.len();
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {}  ", key),
                            Style::default().fg(theme.accent).bg(bg),
                        ),
                        Span::styled(
                            val_str,
                            Style::default().fg(theme.fg).bg(bg),
                        ),
                        if is_selected {
                            Span::styled(
                                "  ← c to copy",
                                Style::default().fg(theme.fg_dim).bg(bg),
                            )
                        } else {
                            Span::raw("")
                        },
                    ]));
                    click_ranges.push((attr_line_idx, 1, attr_idx));
                }
            } else {
                // Not an object — show raw
                lines.push(Line::from(Span::styled(
                    format!("  {}", attrs_str),
                    Style::default().fg(theme.fg),
                )));
            }
        } else {
            // Failed to parse — show raw
            lines.push(Line::from(Span::styled(
                format!("  {}", attrs_str),
                Style::default().fg(theme.fg),
            )));
        }
    }

    app.log_detail_copyable_count = copyable_count;

    // Compute click areas based on line positions and scroll offset
    let scroll = app.log_detail_scroll as u16;
    app.log_detail_click_areas.clear();
    for (line_start, line_count, copyable_idx) in &click_ranges {
        let start_line = *line_start as u16;
        let count = *line_count as u16;
        // Screen Y = inner.y + (line_in_content - scroll)
        if start_line + count > scroll && start_line < scroll + inner.height {
            let visible_start = if start_line >= scroll { start_line - scroll } else { 0 };
            let visible_end = ((start_line + count).saturating_sub(scroll)).min(inner.height);
            if visible_start < visible_end {
                let rect = Rect::new(
                    inner.x,
                    inner.y + visible_start,
                    inner.width,
                    visible_end - visible_start,
                );
                app.log_detail_click_areas.push((rect, *copyable_idx));
            }
        }
    }

    // Render with scroll offset
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme.bg_surface));

    f.render_widget(paragraph, inner);
}
