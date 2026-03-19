use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::theme::Theme;
use crate::ui::widgets::button::Button;
use crate::ui::widgets::numeric_input::{NumericInput, NumericInputState};

/// Focus positions within the date range picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DateRangePickerFocus {
    StartYear = 0,
    StartMonth = 1,
    StartDay = 2,
    StartHour = 3,
    StartMinute = 4,
    StartSecond = 5,
    EndYear = 6,
    EndMonth = 7,
    EndDay = 8,
    EndHour = 9,
    EndMinute = 10,
    EndSecond = 11,
    Apply = 12,
    Cancel = 13,
    Now = 14,
}

const FIELDS_PER_ROW: u8 = 6;

impl DateRangePickerFocus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::StartYear,
            1 => Self::StartMonth,
            2 => Self::StartDay,
            3 => Self::StartHour,
            4 => Self::StartMinute,
            5 => Self::StartSecond,
            6 => Self::EndYear,
            7 => Self::EndMonth,
            8 => Self::EndDay,
            9 => Self::EndHour,
            10 => Self::EndMinute,
            11 => Self::EndSecond,
            12 => Self::Apply,
            13 => Self::Cancel,
            14 => Self::Now,
            _ => Self::Apply,
        }
    }

    fn as_u8(self) -> u8 {
        self as u8
    }

    /// Move to the next row, keeping the column position where possible.
    fn next_row(self) -> Self {
        let idx = self.as_u8();
        if idx < FIELDS_PER_ROW {
            // Start → End (same column)
            Self::from_u8(idx + FIELDS_PER_ROW)
        } else if idx < FIELDS_PER_ROW * 2 {
            // End → Buttons (clamp to button range)
            Self::Apply
        } else {
            // Buttons → Start (first field)
            Self::StartYear
        }
    }

    /// Move to the previous row, keeping the column position where possible.
    fn prev_row(self) -> Self {
        let idx = self.as_u8();
        if idx < FIELDS_PER_ROW {
            // Start → Buttons
            Self::Apply
        } else if idx < FIELDS_PER_ROW * 2 {
            // End → Start (same column)
            Self::from_u8(idx - FIELDS_PER_ROW)
        } else {
            // Buttons → End (first field)
            Self::EndYear
        }
    }

    fn is_button(self) -> bool {
        self.as_u8() >= 12
    }

    /// Move right within the same row.
    fn right(self) -> Self {
        match self {
            Self::StartYear => Self::StartMonth,
            Self::StartMonth => Self::StartDay,
            Self::StartDay => Self::StartHour,
            Self::StartHour => Self::StartMinute,
            Self::StartMinute => Self::StartSecond,
            Self::StartSecond => Self::StartYear,
            Self::EndYear => Self::EndMonth,
            Self::EndMonth => Self::EndDay,
            Self::EndDay => Self::EndHour,
            Self::EndHour => Self::EndMinute,
            Self::EndMinute => Self::EndSecond,
            Self::EndSecond => Self::EndYear,
            Self::Apply => Self::Cancel,
            Self::Cancel => Self::Now,
            Self::Now => Self::Apply,
        }
    }

    /// Move left within the same row.
    fn left(self) -> Self {
        match self {
            Self::StartYear => Self::StartSecond,
            Self::StartMonth => Self::StartYear,
            Self::StartDay => Self::StartMonth,
            Self::StartHour => Self::StartDay,
            Self::StartMinute => Self::StartHour,
            Self::StartSecond => Self::StartMinute,
            Self::EndYear => Self::EndSecond,
            Self::EndMonth => Self::EndYear,
            Self::EndDay => Self::EndMonth,
            Self::EndHour => Self::EndDay,
            Self::EndMinute => Self::EndHour,
            Self::EndSecond => Self::EndMinute,
            Self::Apply => Self::Now,
            Self::Cancel => Self::Apply,
            Self::Now => Self::Cancel,
        }
    }
}

/// Action returned by the picker after handling an event.
pub enum DateRangePickerAction {
    Applied(i64, i64),
    Cancelled,
    Noop,
}

/// State for the date/time range picker popup.
pub struct DateRangePickerState {
    pub start: [NumericInputState; 6], // year, month, day, hour, minute, second
    pub end: [NumericInputState; 6],
    pub focus: DateRangePickerFocus,
    pub field_areas: Vec<(DateRangePickerFocus, Rect)>,
    popup_area: Option<Rect>,
    hovered: Option<DateRangePickerFocus>,
}

impl DateRangePickerState {
    pub fn new(start_ms: i64, end_ms: i64) -> Self {
        let (sy, smo, sd, sh, smi, ss) = ms_to_fields(start_ms);
        let (ey, emo, ed, eh, emi, es) = ms_to_fields(end_ms);
        Self {
            start: [
                NumericInputState::new(sy, 2000, 2099, 4),
                NumericInputState::new(smo, 1, 12, 2),
                NumericInputState::new(sd, 1, 31, 2),
                NumericInputState::new(sh, 0, 23, 2),
                NumericInputState::new(smi, 0, 59, 2),
                NumericInputState::new(ss, 0, 59, 2),
            ],
            end: [
                NumericInputState::new(ey, 2000, 2099, 4),
                NumericInputState::new(emo, 1, 12, 2),
                NumericInputState::new(ed, 1, 31, 2),
                NumericInputState::new(eh, 0, 23, 2),
                NumericInputState::new(emi, 0, 59, 2),
                NumericInputState::new(es, 0, 59, 2),
            ],
            focus: DateRangePickerFocus::StartHour,
            field_areas: Vec::new(),
            popup_area: None,
            hovered: None,
        }
    }

    /// Assemble start fields into a timestamp in milliseconds.
    pub fn start_ms(&self) -> i64 {
        fields_to_ms(
            self.start[0].value,
            self.start[1].value,
            self.start[2].value,
            self.start[3].value,
            self.start[4].value,
            self.start[5].value,
        )
    }

    /// Assemble end fields into a timestamp in milliseconds.
    pub fn end_ms(&self) -> i64 {
        fields_to_ms(
            self.end[0].value,
            self.end[1].value,
            self.end[2].value,
            self.end[3].value,
            self.end[4].value,
            self.end[5].value,
        )
    }

    /// Fill end fields with current local time.
    pub fn set_end_to_now(&mut self) {
        let now = Local::now();
        self.end[0].value = now.year();
        self.end[1].value = now.month() as i32;
        self.end[2].value = now.day() as i32;
        self.end[3].value = now.hour() as i32;
        self.end[4].value = now.minute() as i32;
        self.end[5].value = now.second() as i32;
    }

    /// Commit typing buffer on the currently focused field (if any).
    fn commit_focused(&mut self) {
        if let Some(input) = self.focused_input_mut() {
            input.commit_typing();
        }
    }

    fn focused_input_mut(&mut self) -> Option<&mut NumericInputState> {
        let idx = self.focus.as_u8();
        if idx < FIELDS_PER_ROW {
            Some(&mut self.start[idx as usize])
        } else if idx < FIELDS_PER_ROW * 2 {
            Some(&mut self.end[(idx - FIELDS_PER_ROW) as usize])
        } else {
            None
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DateRangePickerAction {
        match key.code {
            KeyCode::Esc => {
                return DateRangePickerAction::Cancelled;
            }
            KeyCode::Tab => {
                self.commit_focused();
                self.focus = self.focus.next_row();
            }
            KeyCode::BackTab => {
                self.commit_focused();
                self.focus = self.focus.prev_row();
            }
            KeyCode::Left => {
                self.commit_focused();
                self.focus = self.focus.left();
            }
            KeyCode::Right => {
                self.commit_focused();
                self.focus = self.focus.right();
            }
            KeyCode::Up => {
                if let Some(input) = self.focused_input_mut() {
                    input.increment();
                }
            }
            KeyCode::Down => {
                if let Some(input) = self.focused_input_mut() {
                    input.decrement();
                }
            }
            KeyCode::Enter => {
                match self.focus {
                    DateRangePickerFocus::Apply => {
                        self.commit_focused();
                        return DateRangePickerAction::Applied(self.start_ms(), self.end_ms());
                    }
                    DateRangePickerFocus::Cancel => {
                        return DateRangePickerAction::Cancelled;
                    }
                    DateRangePickerFocus::Now => {
                        self.set_end_to_now();
                    }
                    _ => {
                        // Commit and move to next field in the same row
                        self.commit_focused();
                        self.focus = self.focus.right();
                    }
                }
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                if let Some(input) = self.focused_input_mut() {
                    input.handle_digit(ch);
                }
            }
            _ => {}
        }
        DateRangePickerAction::Noop
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> DateRangePickerAction {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let col = mouse.column;
                let row = mouse.row;

                // Check if click is on any field/button
                for &(focus, rect) in &self.field_areas {
                    if col >= rect.x
                        && col < rect.x + rect.width
                        && row >= rect.y
                        && row < rect.y + rect.height
                    {
                        self.commit_focused();
                        self.focus = focus;
                        // Execute button actions immediately on click
                        match focus {
                            DateRangePickerFocus::Apply => {
                                return DateRangePickerAction::Applied(
                                    self.start_ms(),
                                    self.end_ms(),
                                );
                            }
                            DateRangePickerFocus::Cancel => {
                                return DateRangePickerAction::Cancelled;
                            }
                            DateRangePickerFocus::Now => {
                                self.set_end_to_now();
                            }
                            _ => {}
                        }
                        return DateRangePickerAction::Noop;
                    }
                }

                // Click outside popup → cancel
                if let Some(popup) = self.popup_area {
                    if col < popup.x
                        || col >= popup.x + popup.width
                        || row < popup.y
                        || row >= popup.y + popup.height
                    {
                        return DateRangePickerAction::Cancelled;
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                let col = mouse.column;
                let row = mouse.row;
                for &(focus, rect) in &self.field_areas {
                    if col >= rect.x
                        && col < rect.x + rect.width
                        && row >= rect.y
                        && row < rect.y + rect.height
                        && !focus.is_button()
                    {
                        self.commit_focused();
                        self.focus = focus;
                        if let Some(input) = self.focused_input_mut() {
                            input.increment();
                        }
                        return DateRangePickerAction::Noop;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                let col = mouse.column;
                let row = mouse.row;
                for &(focus, rect) in &self.field_areas {
                    if col >= rect.x
                        && col < rect.x + rect.width
                        && row >= rect.y
                        && row < rect.y + rect.height
                        && !focus.is_button()
                    {
                        self.commit_focused();
                        self.focus = focus;
                        if let Some(input) = self.focused_input_mut() {
                            input.decrement();
                        }
                        return DateRangePickerAction::Noop;
                    }
                }
            }
            MouseEventKind::Moved => {
                let col = mouse.column;
                let row = mouse.row;
                self.hovered = None;
                for &(focus, rect) in &self.field_areas {
                    if col >= rect.x
                        && col < rect.x + rect.width
                        && row >= rect.y
                        && row < rect.y + rect.height
                    {
                        self.hovered = Some(focus);
                        break;
                    }
                }
            }
            _ => {}
        }
        DateRangePickerAction::Noop
    }

    pub fn render(&mut self, f: &mut Frame, theme: &Theme) {
        let screen = f.area();
        let popup_w: u16 = 60;
        let popup_h: u16 = 15;

        // Don't render if terminal is too small
        if screen.width < 62 || screen.height < 17 {
            return;
        }

        // Center the popup
        let x = screen.x + (screen.width.saturating_sub(popup_w)) / 2;
        let y = screen.y + (screen.height.saturating_sub(popup_h)) / 2;
        let popup_rect = Rect::new(x, y, popup_w, popup_h);
        self.popup_area = Some(popup_rect);

        // Clear background and draw outer border
        f.render_widget(Clear, popup_rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent).bg(theme.bg_overlay))
            .title(Line::from(vec![Span::styled(
                " Time Range ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )]))
            .style(Style::default().bg(theme.bg_overlay));

        let inner = block.inner(popup_rect);
        f.render_widget(block, popup_rect);

        self.field_areas.clear();

        let left_pad = 3u16;
        let label_w = 7u16;

        // Start row
        let start_fields_y = inner.y + 1;
        self.render_row(
            f,
            theme,
            "Start",
            &[
                DateRangePickerFocus::StartYear,
                DateRangePickerFocus::StartMonth,
                DateRangePickerFocus::StartDay,
                DateRangePickerFocus::StartHour,
                DateRangePickerFocus::StartMinute,
                DateRangePickerFocus::StartSecond,
            ],
            true,
            inner.x + left_pad,
            start_fields_y,
            label_w,
        );

        // End row
        let end_fields_y = start_fields_y + 4;
        self.render_row(
            f,
            theme,
            "End",
            &[
                DateRangePickerFocus::EndYear,
                DateRangePickerFocus::EndMonth,
                DateRangePickerFocus::EndDay,
                DateRangePickerFocus::EndHour,
                DateRangePickerFocus::EndMinute,
                DateRangePickerFocus::EndSecond,
            ],
            false,
            inner.x + left_pad,
            end_fields_y,
            label_w,
        );

        // Buttons row
        let buttons_y = end_fields_y + 4;
        let btn_x = inner.x + left_pad + label_w;
        self.render_buttons(f, theme, btn_x, buttons_y);
    }

    fn render_row(
        &mut self,
        f: &mut Frame,
        theme: &Theme,
        label: &str,
        focuses: &[DateRangePickerFocus; 6],
        is_start: bool,
        x: u16,
        y: u16,
        label_w: u16,
    ) {
        // Render label centered vertically with the fields (fields are 3 rows, label on middle row)
        let label_style = Style::default().fg(theme.fg_dim).bg(theme.bg_overlay);
        let label_para = Paragraph::new(label)
            .style(label_style)
            .alignment(Alignment::Left);
        f.render_widget(
            label_para,
            Rect::new(x, y + 1, label_w, 1),
        );

        let fields = if is_start {
            &self.start
        } else {
            &self.end
        };

        // Render date fields: Year Month Day  Hour:Minute:Second
        let mut fx = x + label_w;
        for (i, &focus) in focuses.iter().enumerate() {
            let field = &fields[i];
            let w = field.width();
            let focused = self.focus == focus;
            let rect = Rect::new(fx, y, w, 3);
            f.render_widget(NumericInput::new(field, focused, theme), rect);
            self.field_areas.push((focus, rect));
            fx += w;

            // Add spacing/separator
            if i == 2 {
                // Gap between day and hour
                fx += 2;
            } else if i == 3 || i == 4 {
                // Colon between hour:minute:second
                let colon_style = Style::default().fg(theme.fg_dim).bg(theme.bg_overlay);
                f.render_widget(
                    Paragraph::new(":").style(colon_style),
                    Rect::new(fx, y + 1, 1, 1),
                );
                fx += 1;
            } else if i < 5 {
                fx += 1;
            }
        }
    }

    fn render_buttons(&mut self, f: &mut Frame, theme: &Theme, x: u16, y: u16) {
        let buttons: &[(DateRangePickerFocus, &str)] = &[
            (DateRangePickerFocus::Apply, "Apply"),
            (DateRangePickerFocus::Cancel, "Cancel"),
            (DateRangePickerFocus::Now, "Now"),
        ];

        let mut bx = x;
        for &(focus, label) in buttons {
            let focused = self.focus == focus;
            let semantic_color = match focus {
                DateRangePickerFocus::Apply => theme.success,
                DateRangePickerFocus::Cancel => theme.error,
                DateRangePickerFocus::Now => theme.accent,
                _ => theme.accent,
            };
            let (fg, border_fg) = if focused {
                (semantic_color, semantic_color)
            } else {
                (theme.fg_dim, theme.border)
            };

            let is_hovered = self.hovered == Some(focus);
            let btn = Button::new(label)
                .fg(fg)
                .bg(theme.bg_overlay)
                .border_fg(border_fg)
                .border_bg(theme.bg_overlay)
                .bold(focused)
                .hovered(is_hovered)
                .hover_fg(semantic_color)
                .hover_border_fg(semantic_color)
                .hover_bg(theme.bg_highlight);

            let w = btn.width();
            let rect = Rect::new(bx, y, w, Button::height());
            f.render_widget(btn, rect);
            self.field_areas.push((focus, rect));
            bx += w + 1;
        }
    }
}

/// Convert milliseconds timestamp to (year, month, day, hour, minute, second) in local time.
fn ms_to_fields(ms: i64) -> (i32, i32, i32, i32, i32, i32) {
    if let Some(dt) = Local.timestamp_millis_opt(ms).single() {
        (
            dt.year(),
            dt.month() as i32,
            dt.day() as i32,
            dt.hour() as i32,
            dt.minute() as i32,
            dt.second() as i32,
        )
    } else {
        (2026, 1, 1, 0, 0, 0)
    }
}

/// Convert (year, month, day, hour, minute, second) in local time to milliseconds timestamp.
/// Clamps day to the last valid day of the month if invalid.
fn fields_to_ms(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> i64 {
    let month = month.clamp(1, 12) as u32;
    let max_day = last_day_of_month(year, month);
    let day = (day as u32).clamp(1, max_day);

    if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
        if let Some(time) = date.and_hms_opt(
            hour.clamp(0, 23) as u32,
            minute.clamp(0, 59) as u32,
            second.clamp(0, 59) as u32,
        ) {
            if let Some(dt) = Local.from_local_datetime(&time).single() {
                return dt.timestamp_millis();
            }
        }
    }

    // Fallback: current time
    Local::now().timestamp_millis()
}

/// Get the last valid day of a given month/year.
fn last_day_of_month(year: i32, month: u32) -> u32 {
    if month == 12 {
        31
    } else if let Some(next) = NaiveDate::from_ymd_opt(year, month + 1, 1) {
        next.pred_opt().map(|d| d.day()).unwrap_or(28)
    } else {
        28
    }
}
