use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::Tab;
use crate::ui::pages::top::app::{ChartMode, FilterMode, LogFocus, TopApp};

pub fn render(f: &mut Frame, area: Rect, app: &TopApp) {
    let theme = &app.theme;
    let key_style = Style::default().fg(theme.accent);
    let text_style = Style::default().fg(theme.fg_dim);
    let sep = Span::styled("  ", text_style);

    let is_text_input = app.tab == Tab::Logs
        && (app.log_focus == LogFocus::Filter || app.log_focus == LogFocus::Detail);

    // Common prefix — skip when a text-input-like focus is active
    let mut spans = if is_text_input {
        Vec::new()
    } else {
        vec![
            Span::styled("q", key_style),
            Span::styled(" Quit", text_style),
            sep.clone(),
            Span::styled("\u{2190}\u{2192}", key_style), // ←→
            Span::styled(" Tab", text_style),
            sep.clone(),
            Span::styled("\u{2191}\u{2193}", key_style), // ↑↓
            Span::styled(" Select", text_style),
        ]
    };

    match app.tab {
        Tab::Dashboard => {
            spans.push(sep.clone());
            spans.push(Span::styled("Enter", key_style));
            spans.push(Span::styled(" Focus", text_style));
            spans.push(sep.clone());
            spans.push(Span::styled("Space", key_style));
            spans.push(Span::styled(" Toggle", text_style));
            spans.push(sep.clone());
            spans.push(Span::styled("Ctrl+A", key_style));
            spans.push(Span::styled(" All", text_style));
            spans.push(sep.clone());
            spans.push(Span::styled("+/-", key_style));
            spans.push(Span::styled(" Refresh", text_style));
        }
        Tab::Logs => {
            match app.log_focus {
                LogFocus::None => {
                    spans.push(sep.clone());
                    spans.push(Span::styled("\u{2190}\u{2192}", key_style));
                    spans.push(Span::styled(" Switch Tab", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Tab", key_style));
                    spans.push(Span::styled(" Focus", text_style));
                }
                LogFocus::Chart => {
                    spans.push(sep.clone());
                    spans.push(Span::styled("\u{2190}\u{2192}", key_style));
                    spans.push(Span::styled(" Cursor", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("S+\u{2190}\u{2192}", key_style));
                    spans.push(Span::styled(" Select", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Enter", key_style));
                    spans.push(Span::styled(" Confirm", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("+/-", key_style));
                    spans.push(Span::styled(" Zoom", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Tab", key_style));
                    if app.has_right("logs:search") {
                        spans.push(Span::styled(" Filter", text_style));
                    } else {
                        spans.push(Span::styled(" Viewer", text_style));
                    }
                }
                LogFocus::Filter => {
                    spans.push(Span::styled("Enter", key_style));
                    spans.push(Span::styled(" Apply", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("C+d", key_style));
                    spans.push(Span::styled(" Clear", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("C+Enter", key_style));
                    spans.push(Span::styled(" Newline", text_style));
                    if app.has_right("logs:search:sql") {
                        spans.push(sep.clone());
                        spans.push(Span::styled("C+t", key_style));
                        let mode_name = match app.log_filter_mode {
                            FilterMode::Dsl => "SQL",
                            FilterMode::Sql => "DSL",
                        };
                        spans.push(Span::styled(format!(" {}", mode_name), text_style));
                    }
                    spans.push(sep.clone());
                    spans.push(Span::styled("\u{2191}\u{2193}", key_style));
                    spans.push(Span::styled(" History", text_style));
                    if app.log_filter_input.has_completions() {
                        spans.push(sep.clone());
                        spans.push(Span::styled("Tab", key_style));
                        spans.push(Span::styled(" Accept", text_style));
                    }
                    spans.push(sep.clone());
                    spans.push(Span::styled("Esc", key_style));
                    spans.push(Span::styled(" Unfocus", text_style));
                }
                LogFocus::Viewer => {
                    spans.push(sep.clone());
                    spans.push(Span::styled("\u{2191}\u{2193}", key_style));
                    spans.push(Span::styled(" Select", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Enter", key_style));
                    spans.push(Span::styled(" Detail", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("PgUp/Dn", key_style));
                    spans.push(Span::styled(" Page", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("+/-", key_style));
                    spans.push(Span::styled(
                        format!(" Buffer ({})", app.buffer_size),
                        text_style,
                    ));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Tab", key_style));
                    spans.push(Span::styled(" Chart", text_style));
                }
                LogFocus::Detail => {
                    spans.push(Span::styled("\u{2191}\u{2193}", key_style));
                    spans.push(Span::styled(" Navigate", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Tab", key_style));
                    spans.push(Span::styled(" Select", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("c", key_style));
                    spans.push(Span::styled(" Copy", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("v", key_style));
                    spans.push(Span::styled(" Context", text_style));
                    spans.push(sep.clone());
                    spans.push(Span::styled("Esc", key_style));
                    spans.push(Span::styled(" Close", text_style));
                }
            }
        }
        Tab::Detail => {
            if app.has_right("start") {
                spans.push(sep.clone());
                spans.push(Span::styled("s", key_style));
                spans.push(Span::styled(" Start", text_style));
            }
            if app.has_right("stop") {
                spans.push(sep.clone());
                spans.push(Span::styled("S", key_style));
                spans.push(Span::styled(" Stop", text_style));
            }
            if app.has_right("restart") {
                spans.push(sep.clone());
                spans.push(Span::styled("r", key_style));
                spans.push(Span::styled(" Restart", text_style));
            }
        }
    }

    // Common suffix — skip when text-input-like focus is active
    if !is_text_input {
        spans.push(sep.clone());
        match app.chart_mode {
            ChartMode::Paused { .. } => {
                spans.push(Span::styled("p", key_style));
                spans.push(Span::styled(" Resume", text_style));
            }
            ChartMode::Live => {
                spans.push(Span::styled("p", key_style));
                spans.push(Span::styled(" Pause", text_style));
            }
        }

        spans.push(sep);
        spans.push(Span::styled("?", key_style));
        spans.push(Span::styled(" Help", text_style));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line)
        .style(Style::default().bg(theme.bg_surface));
    f.render_widget(paragraph, area);
}
