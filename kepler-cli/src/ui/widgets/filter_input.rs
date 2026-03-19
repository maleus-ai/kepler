use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

use crate::ui::pages::top::app::{FilterMode, TopApp};

use super::text_input;

pub fn render(f: &mut Frame, area: Rect, app: &TopApp, focused: bool) {
    let theme = &app.theme;

    let border_color = if app.log_filter_error.is_some() {
        theme.error
    } else if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let mode_label = if app.has_right("logs:search:sql") {
        match app.log_filter_mode {
            FilterMode::Dsl => "Query DSL",
            FilterMode::Sql => "SQL",
        }
    } else {
        "Query DSL"
    };
    let mut title_spans = vec![Span::styled(
        format!(" [{}] ", mode_label),
        Style::default().fg(theme.accent),
    )];

    if let Some(ref err) = app.log_filter_error {
        title_spans.push(Span::styled(
            format!(" {} ", err),
            Style::default().fg(theme.error),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(title_spans))
        .style(Style::default().bg(theme.bg_surface));

    let inner = block.inner(area);
    f.render_widget(block, area);

    text_input::render_inner(f, inner, &app.log_filter_input, focused, theme);
}
