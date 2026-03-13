use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::ui::theme::Theme;
use crate::ui::widgets::utils::centered_rect;

pub fn render_help(f: &mut Frame, theme: &Theme) {
    let area = f.area();

    let dim = Block::default().style(Style::default().bg(theme.bg));
    f.render_widget(Clear, area);
    f.render_widget(dim, area);

    let popup_area = centered_rect(60, 80, area);

    let block = Block::default()
        .title(" \u{25c6} Kepler \u{2014} Keyboard Shortcuts ")
        .title_style(Style::default().fg(theme.fg_bright))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border_focused))
        .style(Style::default().bg(theme.bg_overlay));

    let key_style = Style::default().fg(theme.accent);
    let text_style = Style::default().fg(theme.fg);
    let section_style = Style::default().fg(theme.fg_bright);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("  Navigation", section_style)),
        Line::from(vec![
            Span::styled("    \u{2190}/\u{2192} or Tab  ", key_style),
            Span::styled("Switch tabs (Dashboard / Logs / Detail)", text_style),
        ]),
        Line::from(vec![
            Span::styled("    \u{2191}/\u{2193}         ", key_style),
            Span::styled("Move service selection", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Enter       ", key_style),
            Span::styled("Focus selected service", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Space       ", key_style),
            Span::styled("Toggle service in multi-select", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Esc         ", key_style),
            Span::styled("Unfocus / Close popup", text_style),
        ]),
        Line::from(vec![
            Span::styled("    +/-         ", key_style),
            Span::styled("Adjust refresh rate", text_style),
        ]),
        Line::from(vec![
            Span::styled("    l           ", key_style),
            Span::styled("Resume live mode", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Service Management", section_style)),
        Line::from(vec![
            Span::styled("    s           ", key_style),
            Span::styled("Start selected service(s)", text_style),
        ]),
        Line::from(vec![
            Span::styled("    S           ", key_style),
            Span::styled("Stop selected service(s)", text_style),
        ]),
        Line::from(vec![
            Span::styled("    r           ", key_style),
            Span::styled("Restart selected service(s)", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Log Chart", section_style)),
        Line::from(vec![
            Span::styled("    \u{2190}/\u{2192}         ", key_style),
            Span::styled("Move cursor between buckets", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Shift+\u{2190}/\u{2192}   ", key_style),
            Span::styled("Select range", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Enter       ", key_style),
            Span::styled("Zoom to selection / Show tooltip", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Esc         ", key_style),
            Span::styled("Cancel selection", text_style),
        ]),
        Line::from(vec![
            Span::styled("    +/-         ", key_style),
            Span::styled("Zoom in/out", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Home/End    ", key_style),
            Span::styled("Jump to start/end", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Tab         ", key_style),
            Span::styled("Switch to log viewer", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Log Viewer", section_style)),
        Line::from(vec![
            Span::styled("    \u{2191}/\u{2193}         ", key_style),
            Span::styled("Move cursor to select log line", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Enter       ", key_style),
            Span::styled("Open log detail panel", text_style),
        ]),
        Line::from(vec![
            Span::styled("    PgUp/PgDn   ", key_style),
            Span::styled("Page through log entries", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Log Detail Panel", section_style)),
        Line::from(vec![
            Span::styled("    \u{2191}/\u{2193}         ", key_style),
            Span::styled("Navigate through log entries", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Tab         ", key_style),
            Span::styled("Cycle through copyable items", text_style),
        ]),
        Line::from(vec![
            Span::styled("    c           ", key_style),
            Span::styled("Copy selected item to clipboard", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Click       ", key_style),
            Span::styled("Select and copy log/attribute", text_style),
        ]),
        Line::from(vec![
            Span::styled("    v           ", key_style),
            Span::styled("View log in context (30s window)", text_style),
        ]),
        Line::from(vec![
            Span::styled("    PgUp/PgDn   ", key_style),
            Span::styled("Scroll detail panel", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Esc         ", key_style),
            Span::styled("Close detail panel", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Log Filter", section_style)),
        Line::from(vec![
            Span::styled("    Ctrl+Enter  ", key_style),
            Span::styled("Insert newline", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+D      ", key_style),
            Span::styled("Clear filter", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+T      ", key_style),
            Span::styled("Toggle DSL/SQL mode", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+Space  ", key_style),
            Span::styled("Trigger autocompletion", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Mouse", section_style)),
        Line::from(vec![
            Span::styled("    Click       ", key_style),
            Span::styled("Select service", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Double-click", key_style),
            Span::styled(" Focus service", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Scroll      ", key_style),
            Span::styled("Navigate / Zoom charts", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Right-click ", key_style),
            Span::styled("Context menu", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Drag border ", key_style),
            Span::styled("Resize panes", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Drag chart  ", key_style),
            Span::styled("Pan time range", text_style),
        ]),
        Line::from(vec![
            Span::styled("    Click chart ", key_style),
            Span::styled("Show value tooltip", text_style),
        ]),
        Line::from(""),
        Line::from(Span::styled("  General", section_style)),
        Line::from(vec![
            Span::styled("    ?           ", key_style),
            Span::styled("Toggle this help", text_style),
        ]),
        Line::from(vec![
            Span::styled("    q / Ctrl+C  ", key_style),
            Span::styled("Quit", text_style),
        ]),
        Line::from(""),
    ];

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });

    f.render_widget(paragraph, popup_area);
}
