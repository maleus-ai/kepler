use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::ui::Tab;
use crate::ui::theme::Theme;
use crate::ui::pages::top::app::ChartMode;

const TABS: &[(Tab, &str)] = &[
    (Tab::Dashboard, "Dashboard"),
    (Tab::Logs, "Logs"),
    (Tab::Detail, "Detail"),
];

/// Render the tab bar as floating rounded containers, plus a live/paused indicator at far right.
/// Returns (tab click areas, indicator click area).
pub fn render(
    f: &mut Frame,
    area: Rect,
    active: Tab,
    chart_mode: ChartMode,
    refresh_secs: u64,
    theme: &Theme,
) -> (Vec<(Tab, Rect)>, Option<Rect>) {
    if area.height < 3 || area.width < 10 {
        return (Vec::new(), None);
    }

    let mut x = area.x;
    let mut tab_rects = Vec::new();

    // Render tab containers
    for &(tab, label) in TABS {
        let content_width = label.len() as u16 + 2;
        let tab_width = content_width + 2;

        if x + tab_width > area.x + area.width {
            break;
        }

        let is_active = tab == active;

        let border_fg = if is_active {
            theme.border_focused
        } else {
            theme.bg_surface
        };

        let text_fg = if is_active { theme.accent } else { theme.fg_dim };
        let text_modifier = if is_active { Modifier::BOLD } else { Modifier::empty() };

        let tab_rect = Rect::new(x, area.y, tab_width, 3);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_fg).bg(theme.bg_mantle))
            .style(Style::default().bg(theme.bg_surface));

        let text_style = Style::default()
            .fg(text_fg)
            .bg(theme.bg_surface)
            .add_modifier(text_modifier);

        let paragraph = Paragraph::new(Line::from(label))
            .style(text_style)
            .alignment(Alignment::Center)
            .block(block);

        f.render_widget(paragraph, tab_rect);
        tab_rects.push((tab, tab_rect));

        x += tab_width + 1;
    }

    // Right-side containers: [Refresh: Xs] [● LIVE/PAUSED]
    // Compute sizes from right to left
    let (indicator_label, indicator_color) = match chart_mode {
        ChartMode::Live => ("\u{25cf} LIVE", theme.success),
        ChartMode::Paused { .. } => ("\u{25cf} PAUSED", theme.warning),
    };
    let indicator_w = indicator_label.len() as u16 + 4; // +2 padding +2 borders

    let refresh_label = format!("Refresh: {}s", refresh_secs);
    let refresh_w = refresh_label.len() as u16 + 4;

    let right_edge = area.x + area.width;
    let total_right = indicator_w + 1 + refresh_w; // +1 gap between them

    let indicator_rect = if right_edge >= area.x + total_right {
        // Live/Paused indicator (far right)
        let ix = right_edge - indicator_w;
        let indicator_rect = Rect::new(ix, area.y, indicator_w, 3);

        let border_fg = match chart_mode {
            ChartMode::Live => theme.success,
            ChartMode::Paused { .. } => theme.warning,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_fg).bg(theme.bg_mantle))
            .style(Style::default().bg(theme.bg_surface));

        let text_style = Style::default()
            .fg(indicator_color)
            .bg(theme.bg_surface)
            .add_modifier(Modifier::BOLD);

        let paragraph = Paragraph::new(Line::from(indicator_label))
            .style(text_style)
            .alignment(Alignment::Center)
            .block(block);

        f.render_widget(paragraph, indicator_rect);

        // Refresh container (left of indicator)
        let rx = ix - 1 - refresh_w;
        let refresh_rect = Rect::new(rx, area.y, refresh_w, 3);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.bg_surface).bg(theme.bg_mantle))
            .style(Style::default().bg(theme.bg_surface));

        let text_style = Style::default()
            .fg(theme.fg_dim)
            .bg(theme.bg_surface);

        let paragraph = Paragraph::new(Line::from(refresh_label.as_str()))
            .style(text_style)
            .alignment(Alignment::Center)
            .block(block);

        f.render_widget(paragraph, refresh_rect);

        Some(indicator_rect)
    } else {
        None
    };

    (tab_rects, indicator_rect)
}
