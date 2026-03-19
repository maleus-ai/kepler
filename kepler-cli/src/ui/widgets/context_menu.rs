use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::theme::Theme;

pub fn render(f: &mut Frame, theme: &Theme, x: u16, y: u16, selected: usize, items: &[&str]) {
    let width: u16 = 20;
    let height: u16 = (items.len() as u16) + 2;

    let frame_area = f.area();
    let x = x.min(frame_area.width.saturating_sub(width));
    let y = y.min(frame_area.height.saturating_sub(height));

    let area = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_focused))
        .style(Style::default().bg(theme.bg_overlay));

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if i == selected {
                Line::from(Span::styled(
                    format!("  {} ", item),
                    theme.selection_style(),
                ))
            } else {
                Line::from(Span::styled(
                    format!("  {} ", item),
                    Style::default().fg(theme.fg),
                ))
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);

    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}
