use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

/// A reusable rounded-border button widget (3 rows tall).
///
/// ```text
/// ╭──────────╮
/// │  label   │
/// ╰──────────╯
/// ```
pub struct Button<'a> {
    label: &'a str,
    fg: Color,
    bg: Color,
    border_fg: Color,
    border_bg: Color,
    bold: bool,
    hovered: bool,
    hover_bg: Option<Color>,
    hover_fg: Option<Color>,
    hover_border_fg: Option<Color>,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            fg: Color::White,
            bg: Color::Reset,
            border_fg: Color::White,
            border_bg: Color::Reset,
            bold: false,
            hovered: false,
            hover_bg: None,
            hover_fg: None,
            hover_border_fg: None,
        }
    }

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = color;
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = color;
        self
    }

    pub fn border_fg(mut self, color: Color) -> Self {
        self.border_fg = color;
        self
    }

    pub fn border_bg(mut self, color: Color) -> Self {
        self.border_bg = color;
        self
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    pub fn hover_bg(mut self, color: Color) -> Self {
        self.hover_bg = Some(color);
        self
    }

    pub fn hover_fg(mut self, color: Color) -> Self {
        self.hover_fg = Some(color);
        self
    }

    pub fn hover_border_fg(mut self, color: Color) -> Self {
        self.hover_border_fg = Some(color);
        self
    }

    /// The width this button needs (content + 2 padding + 2 borders).
    pub fn width(&self) -> u16 {
        self.label.len() as u16 + 4
    }

    /// The fixed height of a button (border + content + border).
    pub const fn height() -> u16 {
        3
    }
}

impl Widget for Button<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let (fg, bg, border_fg) = if self.hovered {
            (
                self.hover_fg.unwrap_or(self.fg),
                self.hover_bg.unwrap_or(self.bg),
                self.hover_border_fg.unwrap_or(self.border_fg),
            )
        } else {
            (self.fg, self.bg, self.border_fg)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_fg).bg(self.border_bg))
            .style(Style::default().bg(bg));

        let mut text_style = Style::default().fg(fg).bg(bg);
        if self.bold {
            text_style = text_style.add_modifier(Modifier::BOLD);
        }

        let paragraph = Paragraph::new(Line::from(self.label))
            .style(text_style)
            .alignment(Alignment::Center)
            .block(block);

        paragraph.render(area, buf);
    }
}
