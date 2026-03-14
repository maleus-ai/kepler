use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::pages::top::app::{ChartKind, ChartTooltip};
use crate::ui::theme::Theme;
use crate::ui::widgets::utils::{format_bytes, format_timestamp_label};

pub fn render(f: &mut Frame, tooltip: &ChartTooltip, theme: &Theme) {
    let timestamp_str = format_timestamp_label(tooltip.timestamp_ms);

    let mut lines = vec![Line::from(Span::styled(
        timestamp_str,
        Style::default().fg(theme.fg_bright),
    ))];

    let mut max_width: u16 = 10;

    for (name, value, color) in &tooltip.entries {
        let value_str = match tooltip.chart_kind {
            ChartKind::Cpu => format!("{:.1}%", value),
            ChartKind::Memory => format_bytes(*value as u64),
            ChartKind::LogLevel => format!("{}", *value as u64),
        };
        let line_text = format!("{}: {}", name, value_str);
        let line_len = line_text.len() as u16;
        if line_len + 4 > max_width {
            max_width = line_len + 4;
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", name), Style::default().fg(*color)),
            Span::styled(value_str, Style::default().fg(theme.fg)),
        ]));
    }

    let width = max_width.max(14);
    let height = (lines.len() as u16) + 2;

    let screen = f.area();
    let x = if tooltip.screen_col + width + 2 < screen.x + screen.width {
        tooltip.screen_col + 1
    } else {
        tooltip.screen_col.saturating_sub(width + 1)
    };
    let y = if tooltip.screen_row >= height + 1 {
        tooltip.screen_row - height
    } else {
        tooltip.screen_row + 1
    };

    let area = Rect::new(
        x.max(screen.x),
        y.max(screen.y),
        width.min(screen.width.saturating_sub(x.saturating_sub(screen.x))),
        height.min(screen.height.saturating_sub(y.saturating_sub(screen.y))),
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg_overlay));

    let paragraph = Paragraph::new(lines).block(block);

    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}
