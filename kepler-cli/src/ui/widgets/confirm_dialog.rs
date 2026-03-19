use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::pages::top::app::{AppAction, TopApp};
use crate::ui::widgets::button::Button;

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

    let confirm_selected = confirm.selected == 0;
    let confirm_w = confirm_label.len() as u16 + 4;
    let cancel_selected = confirm.selected == 1;
    let cancel_w = cancel_label.len() as u16 + 4;

    let total_buttons_w = confirm_w + 1 + cancel_w;
    let start_x = inner.x + (inner.width.saturating_sub(total_buttons_w)) / 2;

    let mouse = app.mouse_pos;
    let is_hovered = |rect: Rect| -> bool {
        mouse.0 >= rect.x && mouse.0 < rect.x + rect.width
            && mouse.1 >= rect.y && mouse.1 < rect.y + rect.height
    };

    // Confirm button
    let confirm_rect = Rect::new(start_x, button_y, confirm_w, Button::height());
    f.render_widget(
        Button::new(confirm_label)
            .fg(confirm_color)
            .bg(theme.bg_overlay)
            .border_fg(if confirm_selected { confirm_color } else { theme.bg_overlay })
            .border_bg(theme.bg_overlay)
            .bold(confirm_selected)
            .hovered(is_hovered(confirm_rect))
            .hover_bg(theme.bg_highlight)
            .hover_border_fg(confirm_color),
        confirm_rect,
    );
    app.confirm_button_areas.push((0, confirm_rect));

    // Cancel button
    let cancel_color = theme.error;
    let cancel_x = start_x + confirm_w + 1;
    let cancel_rect = Rect::new(cancel_x, button_y, cancel_w, Button::height());
    f.render_widget(
        Button::new(cancel_label)
            .fg(if cancel_selected { cancel_color } else { theme.fg_dim })
            .bg(theme.bg_overlay)
            .border_fg(if cancel_selected { cancel_color } else { theme.bg_overlay })
            .border_bg(theme.bg_overlay)
            .bold(cancel_selected)
            .hovered(is_hovered(cancel_rect))
            .hover_fg(cancel_color)
            .hover_bg(theme.bg_highlight)
            .hover_border_fg(cancel_color),
        cancel_rect,
    );
    app.confirm_button_areas.push((1, cancel_rect));
}
