use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::pages::top::app::{AppAction, TopApp};

pub fn render(f: &mut Frame, app: &mut TopApp) {
    let theme = &app.theme;
    let confirm = match &app.confirmation {
        Some(c) => c,
        None => return,
    };

    let area = f.area();

    // Dim overlay — renders on top of existing content without clearing it
    let dim = Block::default().style(
        Style::default()
            .bg(ratatui::style::Color::Rgb(0, 0, 0))
            .fg(theme.fg_dim),
    );
    f.render_widget(dim, area);

    // Dialog size — wider to fit container buttons
    let width = 40u16;
    let height = 10u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let dialog_area = Rect::new(x, y, width, height);

    // Clear only the dialog area so the dialog has a clean background
    f.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(format!(" {} ", confirm.title))
        .title_style(Style::default().fg(theme.fg_bright))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_focused).bg(theme.bg))
        .style(Style::default().bg(theme.bg_overlay));

    // Render dialog block
    f.render_widget(block.clone(), dialog_area);
    let inner = block.inner(dialog_area);

    // Message
    let msg_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", confirm.message),
            Style::default().fg(theme.fg),
        )),
    ];
    let msg_paragraph = Paragraph::new(msg_lines)
        .style(Style::default().bg(theme.bg_overlay));
    f.render_widget(msg_paragraph, Rect::new(inner.x, inner.y, inner.width, 3));

    // Buttons — 3-row containers
    let is_destructive = matches!(
        confirm.action,
        AppAction::StopService(_) | AppAction::StopAll
    );
    let confirm_color = if is_destructive {
        theme.error
    } else {
        theme.success
    };

    let button_y = inner.y + 3;
    if button_y + 3 > inner.y + inner.height {
        return;
    }

    app.confirm_button_areas.clear();

    let confirm_label = "Confirm";
    let cancel_label = "Cancel";
    let confirm_w = confirm_label.len() as u16 + 4; // +2 padding +2 border
    let cancel_w = cancel_label.len() as u16 + 4;
    let total_buttons_w = confirm_w + 1 + cancel_w;
    let start_x = inner.x + (inner.width.saturating_sub(total_buttons_w)) / 2;

    // Confirm button
    let confirm_rect = Rect::new(start_x, button_y, confirm_w, 3);
    let (confirm_border_fg, confirm_text_style) = if confirm.selected == 0 {
        (
            confirm_color,
            Style::default()
                .fg(confirm_color)
                .bg(theme.bg_overlay)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            theme.bg_overlay,
            Style::default().fg(confirm_color).bg(theme.bg_overlay),
        )
    };
    let confirm_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(confirm_border_fg).bg(theme.bg_overlay))
        .style(Style::default().bg(theme.bg_overlay));
    let confirm_paragraph = Paragraph::new(Line::from(confirm_label))
        .style(confirm_text_style)
        .alignment(Alignment::Center)
        .block(confirm_block);
    f.render_widget(confirm_paragraph, confirm_rect);
    app.confirm_button_areas.push((0, confirm_rect));

    // Cancel button
    let cancel_x = start_x + confirm_w + 1;
    let cancel_rect = Rect::new(cancel_x, button_y, cancel_w, 3);
    let (cancel_border_fg, cancel_text_style) = if confirm.selected == 1 {
        (
            theme.fg,
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg_overlay)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            theme.bg_overlay,
            Style::default().fg(theme.fg_dim).bg(theme.bg_overlay),
        )
    };
    let cancel_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(cancel_border_fg).bg(theme.bg_overlay))
        .style(Style::default().bg(theme.bg_overlay));
    let cancel_paragraph = Paragraph::new(Line::from(cancel_label))
        .style(cancel_text_style)
        .alignment(Alignment::Center)
        .block(cancel_block);
    f.render_widget(cancel_paragraph, cancel_rect);
    app.confirm_button_areas.push((1, cancel_rect));
}
