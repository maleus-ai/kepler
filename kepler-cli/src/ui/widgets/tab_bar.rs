use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::ui::Tab;
use crate::ui::theme::Theme;
use crate::ui::pages::top::app::ChartMode;
use crate::ui::widgets::button::Button;
use crate::ui::widgets::utils::format_duration_label;

fn is_hovered(rect: Rect, mouse: (u16, u16)) -> bool {
    mouse.0 >= rect.x
        && mouse.0 < rect.x + rect.width
        && mouse.1 >= rect.y
        && mouse.1 < rect.y + rect.height
}

/// Render the tab bar as floating rounded containers, plus a live/paused indicator at far right.
/// Returns (tab click areas, indicator click area, refresh click area, buffer click area, range click area).
pub fn render(
    f: &mut Frame,
    area: Rect,
    active: Tab,
    available_tabs: &[(Tab, &str)],
    chart_mode: ChartMode,
    refresh_secs: u64,
    buffer_size: usize,
    chart_window: Duration,
    mouse_pos: (u16, u16),
    theme: &Theme,
) -> (Vec<(Tab, Rect)>, Option<Rect>, Option<Rect>, Option<Rect>, Option<Rect>) {
    if area.height < 3 || area.width < 10 {
        return (Vec::new(), None, None, None, None);
    }

    let mut x = area.x;
    let mut tab_rects = Vec::new();

    // Render tab containers
    for &(tab, label) in available_tabs {
        let is_active = tab == active;
        let tab_width = label.len() as u16 + 4; // Button::width() formula
        if x + tab_width > area.x + area.width {
            break;
        }

        let tab_rect = Rect::new(x, area.y, tab_width, Button::height());
        let hovered = !is_active && is_hovered(tab_rect, mouse_pos);
        let btn = Button::new(label)
            .fg(if is_active { theme.accent } else { theme.fg_dim })
            .bg(theme.bg_surface)
            .border_fg(if is_active { theme.border_focused } else { theme.bg_surface })
            .border_bg(theme.bg_mantle)
            .bold(is_active)
            .hovered(hovered)
            .hover_fg(theme.accent)
            .hover_border_fg(theme.accent);
        f.render_widget(btn, tab_rect);
        tab_rects.push((tab, tab_rect));

        x += tab_width + 1;
    }

    // Right-side containers: [Range] [Buffer] [Refresh: Xs] [● LIVE/PAUSED]
    let (indicator_label, indicator_color) = match chart_mode {
        ChartMode::Live => ("\u{25cf} LIVE", theme.success),
        ChartMode::Paused { .. } => ("\u{25cf} PAUSED", theme.warning),
    };
    let indicator_w = indicator_label.len() as u16 + 4;

    let refresh_label = format!("Refresh: {}s", refresh_secs);
    let refresh_w = refresh_label.len() as u16 + 4;

    let buffer_label = if buffer_size >= 1000 {
        format!("Buffer: {}k", buffer_size / 1000)
    } else {
        format!("Buffer: {}", buffer_size)
    };
    let buffer_w = buffer_label.len() as u16 + 4;

    let range_label = match chart_mode {
        ChartMode::Live => {
            format!("\u{23f1} {}", format_duration_label(chart_window.as_secs()))
        }
        ChartMode::Paused { right_edge_ms } => {
            use chrono::{Datelike, Local, TimeZone};
            let left_ms = right_edge_ms - chart_window.as_millis() as i64;
            let left_dt = Local.timestamp_millis_opt(left_ms).single();
            let right_dt = Local.timestamp_millis_opt(right_edge_ms).single();
            match (left_dt, right_dt) {
                (Some(l), Some(r)) if l.day() == r.day() && l.month() == r.month() && l.year() == r.year() => {
                    format!("{} \u{2014} {}", l.format("%H:%M:%S"), r.format("%H:%M:%S"))
                }
                (Some(l), Some(r)) => {
                    format!("{} \u{2014} {}", l.format("%d/%m %H:%M:%S"), r.format("%d/%m %H:%M:%S"))
                }
                _ => String::new(),
            }
        }
    };
    let range_w = range_label.len() as u16 + 4;

    let right_edge = area.x + area.width;
    let total_right = indicator_w + 1 + refresh_w + 1 + buffer_w + 1 + range_w;

    let (indicator_rect, refresh_rect, buffer_rect, range_rect) = if right_edge >= area.x + total_right {
        let ix = right_edge - indicator_w;
        let indicator_rect = Rect::new(ix, area.y, indicator_w, Button::height());
        f.render_widget(
            Button::new(indicator_label)
                .fg(indicator_color)
                .bg(theme.bg_surface)
                .border_fg(indicator_color)
                .border_bg(theme.bg_mantle)
                .bold(true)
                .hovered(is_hovered(indicator_rect, mouse_pos))
                .hover_bg(theme.bg_highlight),
            indicator_rect,
        );

        let rx = ix - 1 - refresh_w;
        let refresh_rect = Rect::new(rx, area.y, refresh_w, Button::height());
        f.render_widget(
            Button::new(&refresh_label)
                .fg(theme.fg_dim)
                .bg(theme.bg_surface)
                .border_fg(theme.bg_surface)
                .border_bg(theme.bg_mantle)
                .hovered(is_hovered(refresh_rect, mouse_pos))
                .hover_fg(theme.fg)
                .hover_border_fg(theme.accent),
            refresh_rect,
        );

        let bx = rx - 1 - buffer_w;
        let buffer_rect = Rect::new(bx, area.y, buffer_w, Button::height());
        f.render_widget(
            Button::new(&buffer_label)
                .fg(theme.fg_dim)
                .bg(theme.bg_surface)
                .border_fg(theme.bg_surface)
                .border_bg(theme.bg_mantle)
                .hovered(is_hovered(buffer_rect, mouse_pos))
                .hover_fg(theme.fg)
                .hover_border_fg(theme.accent),
            buffer_rect,
        );

        let rgx = bx - 1 - range_w;
        let range_rect = Rect::new(rgx, area.y, range_w, Button::height());
        f.render_widget(
            Button::new(&range_label)
                .fg(theme.fg_dim)
                .bg(theme.bg_surface)
                .border_fg(theme.bg_surface)
                .border_bg(theme.bg_mantle)
                .hovered(is_hovered(range_rect, mouse_pos))
                .hover_fg(theme.fg)
                .hover_border_fg(theme.accent),
            range_rect,
        );

        (Some(indicator_rect), Some(refresh_rect), Some(buffer_rect), Some(range_rect))
    } else {
        (None, None, None, None)
    };

    (tab_rects, indicator_rect, refresh_rect, buffer_rect, range_rect)
}
