use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::ui::pages::top::app::{LogFocus, TopApp};
use crate::ui::widgets::button::Button;
use crate::ui::widgets::utils::{format_timestamp_label, word_wrap};

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

    // ── Button bar — wraps to multiple rows if panel is narrow ──────
    let button_h = Button::height();
    let buttons: &[(&str, &str, &str)] = &[
        ("v", "View in context", "context"),
        ("c", "Copy selected", "copy"),
        ("Esc", "Close", "close"),
    ];
    let gap_bg = theme.bg_surface;
    let right_edge = inner.x + inner.width;
    let mut bx = inner.x + 1;
    let mut by = inner.y;

    let mouse = app.mouse_pos;
    app.log_detail_button_areas.clear();
    if inner.height > button_h {
        for (shortcut, label, action) in buttons {
            let content = format!("[{}] {}", shortcut, label);
            let w = content.len() as u16 + 4;
            // Wrap to next row if this button doesn't fit
            if bx + w > right_edge && bx > inner.x + 1 {
                bx = inner.x + 1;
                by += button_h;
                if by + button_h > inner.y + inner.height {
                    break;
                }
            }
            let rect = Rect::new(bx, by, w, button_h);
            let hovered = mouse.0 >= rect.x && mouse.0 < rect.x + rect.width
                && mouse.1 >= rect.y && mouse.1 < rect.y + rect.height;
            let btn_color = match *action {
                "close" => theme.error,
                "copy" => theme.success,
                _ => theme.accent,
            };
            let btn = Button::new(&content)
                .fg(btn_color)
                .bg(theme.bg_surface)
                .border_fg(btn_color)
                .border_bg(gap_bg)
                .hovered(hovered)
                .hover_bg(theme.bg_highlight);
            f.render_widget(btn, rect);
            app.log_detail_button_areas.push((action, rect));
            bx += w + 1;
        }
    }
    let total_button_h = by + button_h - inner.y;

    // Content area below buttons
    let content_area = if inner.height > total_button_h {
        Rect::new(inner.x, inner.y + total_button_h, inner.width, inner.height - total_button_h)
    } else {
        return;
    };

    let entry = match &app.log_detail_entry {
        Some(e) => e.clone(),
        None => {
            let msg = Paragraph::new("No log entry selected")
                .style(Style::default().fg(theme.fg_dim));
            f.render_widget(msg, content_area);
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
    let inner_w = content_area.width.saturating_sub(2) as usize; // padding

    let mut lines: Vec<Line> = Vec::new();
    // Track (line_start, line_count, copyable_index) for click area computation
    let mut click_ranges: Vec<(usize, usize, usize)> = Vec::new();

    // ── Metadata ────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(" Metadata", section_style)));
    lines.push(Line::from(""));

    let ts_str = format_timestamp_label(entry.timestamp);
    let meta_key_w = if entry.hook.is_some() { 9 } else { 9 }; // "Timestamp" = 9
    lines.push(Line::from(vec![
        Span::styled(format!("  {:>width$}  ", "Timestamp", width = meta_key_w), label_style),
        Span::styled(ts_str, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  {:>width$}  ", "Service", width = meta_key_w), label_style),
        Span::styled(&entry.service, Style::default().fg(theme.accent)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  {:>width$}  ", "Level", width = meta_key_w), label_style),
        Span::styled(&entry.level, value_style),
    ]));
    if let Some(ref hook) = entry.hook {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:>width$}  ", "Hook", width = meta_key_w), label_style),
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
        if text.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (empty)",
                Style::default().fg(theme.fg_dim).bg(raw_bg),
            )));
        } else {
            for wrapped in word_wrap(text, wrap_w) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", wrapped),
                    Style::default().fg(theme.fg).bg(raw_bg),
                )));
            }
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
                let max_key_len = obj.keys().map(|k| k.len()).max().unwrap_or(0);
                // Prefix: "  " + right-aligned key + "  "
                let prefix_len = 2 + max_key_len + 2;
                let val_wrap_w = inner_w.saturating_sub(prefix_len);
                let indent: String = " ".repeat(prefix_len);

                for (i, (key, value)) in obj.iter().enumerate() {
                    let attr_idx = i + 1; // 1-based (0 is raw text)
                    let is_selected = selected == attr_idx;
                    let bg = if is_selected { highlight_bg } else { theme.bg_surface };
                    let val_fg = Style::default().fg(theme.fg).bg(bg);
                    copyable_count += 1;

                    let val_str = match value.as_str() {
                        Some(s) => s.to_string(),
                        None => value.to_string(),
                    };

                    let attr_line_idx = lines.len();

                    // Word-wrap the value
                    let wrapped = word_wrap(&val_str, if val_wrap_w > 0 { val_wrap_w } else { val_str.len() });
                    for (wi, wline) in wrapped.iter().enumerate() {
                        if wi == 0 {
                            lines.push(Line::from(vec![
                                Span::styled(
                                    format!("  {:>width$}  ", key, width = max_key_len),
                                    Style::default().fg(theme.accent).bg(bg),
                                ),
                                Span::styled(wline.clone(), val_fg),
                            ]));
                        } else {
                            lines.push(Line::from(vec![
                                Span::styled(indent.clone(), Style::default().bg(bg)),
                                Span::styled(wline.clone(), val_fg),
                            ]));
                        }
                    }

                    // Copy hint on last line if selected
                    if is_selected {
                        if let Some(last) = lines.last_mut() {
                            last.spans.push(Span::styled(
                                "  ← c to copy",
                                Style::default().fg(theme.fg_dim).bg(bg),
                            ));
                        }
                    }

                    let attr_line_count = lines.len() - attr_line_idx;
                    click_ranges.push((attr_line_idx, attr_line_count, attr_idx));
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
        // Screen Y = content_area.y + (line_in_content - scroll)
        if start_line + count > scroll && start_line < scroll + content_area.height {
            let visible_start = if start_line >= scroll { start_line - scroll } else { 0 };
            let visible_end = ((start_line + count).saturating_sub(scroll)).min(content_area.height);
            if visible_start < visible_end {
                let rect = Rect::new(
                    content_area.x,
                    content_area.y + visible_start,
                    content_area.width,
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

    f.render_widget(paragraph, content_area);
}
