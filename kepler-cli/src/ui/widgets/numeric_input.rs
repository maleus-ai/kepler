use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use ratatui::layout::Alignment;

use crate::ui::theme::Theme;

/// A small bordered numeric input that displays a zero-padded integer.
/// Supports typing digits, scroll/arrow increment/decrement, and value wrapping.
pub struct NumericInputState {
    pub value: i32,
    pub min: i32,
    pub max: i32,
    pub digits: u16,
    typing_buffer: Option<String>,
}

impl NumericInputState {
    pub fn new(value: i32, min: i32, max: i32, digits: u16) -> Self {
        Self {
            value: value.clamp(min, max),
            min,
            max,
            digits,
            typing_buffer: None,
        }
    }

    /// Increment value, wrapping from max to min.
    pub fn increment(&mut self) {
        self.commit_typing();
        if self.value >= self.max {
            self.value = self.min;
        } else {
            self.value += 1;
        }
    }

    /// Decrement value, wrapping from min to max.
    pub fn decrement(&mut self) {
        self.commit_typing();
        if self.value <= self.min {
            self.value = self.max;
        } else {
            self.value -= 1;
        }
    }

    /// Append a digit character to the typing buffer.
    /// Auto-commits when buffer length reaches `digits`.
    pub fn handle_digit(&mut self, ch: char) {
        let buf = self.typing_buffer.get_or_insert_with(String::new);
        buf.push(ch);
        if buf.len() >= self.digits as usize {
            self.commit_typing();
        }
    }

    /// Parse the typing buffer, clamp to min/max, and clear it.
    pub fn commit_typing(&mut self) {
        if let Some(buf) = self.typing_buffer.take() {
            if let Ok(v) = buf.parse::<i32>() {
                self.value = v.clamp(self.min, self.max);
            }
        }
    }

    /// Returns the display text: either the in-progress buffer or the zero-padded value.
    pub fn display_text(&self) -> String {
        if let Some(ref buf) = self.typing_buffer {
            // Pad the buffer on the left with spaces to fill digit width
            format!("{:>width$}", buf, width = self.digits as usize)
        } else {
            format!("{:0>width$}", self.value, width = self.digits as usize)
        }
    }

    /// Width needed for this input: digits + 2 padding + 2 borders.
    pub fn width(&self) -> u16 {
        self.digits + 4
    }
}

/// Render a numeric input field (3 rows tall).
pub struct NumericInput<'a> {
    state: &'a NumericInputState,
    focused: bool,
    theme: &'a Theme,
}

impl<'a> NumericInput<'a> {
    pub fn new(state: &'a NumericInputState, focused: bool, theme: &'a Theme) -> Self {
        Self {
            state,
            focused,
            theme,
        }
    }
}

impl Widget for NumericInput<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let border_color = if self.focused {
            self.theme.accent
        } else {
            self.theme.border
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color).bg(self.theme.bg_overlay))
            .style(Style::default().bg(self.theme.bg_overlay));

        let mut text_style = Style::default()
            .fg(if self.focused {
                self.theme.fg
            } else {
                self.theme.fg_dim
            })
            .bg(self.theme.bg_overlay);
        if self.focused {
            text_style = text_style.add_modifier(Modifier::BOLD);
        }

        let text = self.state.display_text();
        let paragraph = Paragraph::new(Line::from(text))
            .style(text_style)
            .alignment(Alignment::Center)
            .block(block);

        paragraph.render(area, buf);
    }
}
