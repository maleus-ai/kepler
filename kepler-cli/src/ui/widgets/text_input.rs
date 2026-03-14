use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::theme::Theme;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TextInputConfig {
    /// Whether to visually wrap long lines at the widget width.
    pub wrapping: bool,
    /// Whether Shift+Enter inserts a newline (multi-line mode).
    pub multiline: bool,
    /// Placeholder text shown when empty and unfocused.
    pub placeholder: String,
}

impl Default for TextInputConfig {
    fn default() -> Self {
        Self {
            wrapping: true,
            multiline: true,
            placeholder: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Action enum returned by handle_key
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputAction {
    /// User pressed Enter — caller should handle submission.
    Submit,
    /// User pressed Esc (no completions) — caller should unfocus.
    Cancel,
    /// User pressed Tab (no completions) — move focus forward.
    FocusNext,
    /// User pressed Shift+Tab — move focus backward.
    FocusPrev,
    /// Autocompletion requested (Ctrl+Space) — caller should provide completions.
    CompletionRequested,
    /// Text content changed — caller may update completions or dirty flags.
    Changed,
    /// No externally visible effect (cursor movement, etc.).
    Noop,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct TextInputState {
    pub config: TextInputConfig,
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub history: Vec<String>,
    history_idx: Option<usize>,
    stash: Option<Vec<String>>,
    completions: Vec<String>,
    completion_idx: usize,
    /// Start column in the cursor line where the typed prefix begins.
    /// Set by the caller via `set_completions()`.
    completion_replace_start: usize,
}

impl TextInputState {
    pub fn new(config: TextInputConfig) -> Self {
        Self {
            config,
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            history: Vec::new(),
            history_idx: None,
            stash: None,
            completions: Vec::new(),
            completion_idx: 0,
            completion_replace_start: 0,
        }
    }

    /// Join all lines into a single string (separated by spaces).
    pub fn text(&self) -> String {
        self.lines.join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Reset to a single empty line.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.completions.clear();
        self.completion_idx = 0;
        self.history_idx = None;
        self.stash = None;
    }

    /// Set completion suggestions. `replace_start` is the column in the cursor
    /// line where the typed prefix begins — used for ghost text and Tab accept.
    pub fn set_completions(&mut self, completions: Vec<String>, replace_start: usize) {
        self.completions = completions;
        self.completion_idx = 0;
        self.completion_replace_start = replace_start;
    }

    #[allow(dead_code)]
    pub fn has_completions(&self) -> bool {
        !self.completions.is_empty()
    }

    #[allow(dead_code)]
    pub fn completions(&self) -> &[String] {
        &self.completions
    }

    #[allow(dead_code)]
    pub fn completion_idx(&self) -> usize {
        self.completion_idx
    }

    /// Compute the ghost text suffix for the currently selected completion.
    pub fn ghost_text(&self) -> Option<&str> {
        if self.completions.is_empty() {
            return None;
        }
        let completion = &self.completions[self.completion_idx];
        let col = self.cursor_col.min(self.lines[self.cursor_row].len());
        let typed_len = col.saturating_sub(self.completion_replace_start);
        if completion.len() > typed_len {
            Some(&completion[typed_len..])
        } else {
            None
        }
    }

    /// Compute the visual height (in rows) needed for content given an inner width.
    pub fn visual_height(&self, inner_width: u16) -> u16 {
        if inner_width == 0 {
            return 1;
        }
        let w = inner_width as usize;
        self.lines
            .iter()
            .map(|line| {
                if line.is_empty() {
                    1
                } else {
                    ((line.len() + w - 1) / w) as u16
                }
            })
            .sum::<u16>()
            .max(1)
    }

    /// Add a string to the input history (skips duplicates of the last entry).
    pub fn push_history(&mut self, text: String) {
        if self.history.last().map(|h| h.as_str()) != Some(text.as_str()) {
            self.history.push(text);
        }
        self.history_idx = None;
        self.stash = None;
    }

    /// Process a key event. Returns an action the caller should handle.
    pub fn handle_key(&mut self, key: KeyEvent) -> TextInputAction {
        // ── Autocompletion navigation (when completions visible) ─────────
        if !self.completions.is_empty() {
            match key.code {
                KeyCode::Tab => {
                    return self.accept_completion();
                }
                KeyCode::Up => {
                    if self.completion_idx > 0 {
                        self.completion_idx -= 1;
                    } else {
                        self.completion_idx = self.completions.len() - 1;
                    }
                    return TextInputAction::Noop;
                }
                KeyCode::Down => {
                    self.completion_idx =
                        (self.completion_idx + 1) % self.completions.len();
                    return TextInputAction::Noop;
                }
                KeyCode::Esc => {
                    self.completions.clear();
                    return TextInputAction::Noop;
                }
                _ => {
                    // Dismiss completions and fall through to normal handling
                    self.completions.clear();
                }
            }
        }

        // ── Ctrl+key shortcuts ───────────────────────────────────────────
        if let KeyCode::Char(c) = key.code {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return match c {
                    'a' => {
                        self.cursor_col = 0;
                        TextInputAction::Noop
                    }
                    'e' => {
                        self.cursor_col = self.lines[self.cursor_row].len();
                        TextInputAction::Noop
                    }
                    ' ' => TextInputAction::CompletionRequested,
                    'c' => TextInputAction::Noop, // swallow Ctrl+C
                    _ => TextInputAction::Noop,
                };
            }
        }

        // ── Normal key handling ──────────────────────────────────────────
        match key.code {
            KeyCode::Char(c) => {
                let line = &mut self.lines[self.cursor_row];
                let col = self.cursor_col.min(line.len());
                line.insert(col, c);
                self.cursor_col = col + 1;
                TextInputAction::Changed
            }

            KeyCode::Backspace => {
                let col = self.cursor_col;
                if col > 0 {
                    let line = &mut self.lines[self.cursor_row];
                    let pos = (col - 1).min(line.len().saturating_sub(1));
                    line.remove(pos);
                    self.cursor_col = pos;
                } else if self.cursor_row > 0 {
                    let current = self.lines.remove(self.cursor_row);
                    self.cursor_row -= 1;
                    let prev_len = self.lines[self.cursor_row].len();
                    self.lines[self.cursor_row].push_str(&current);
                    self.cursor_col = prev_len;
                }
                TextInputAction::Changed
            }

            KeyCode::Delete => {
                let col = self.cursor_col;
                let line = &mut self.lines[self.cursor_row];
                if col < line.len() {
                    line.remove(col);
                    return TextInputAction::Changed;
                } else if self.cursor_row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.cursor_row + 1);
                    self.lines[self.cursor_row].push_str(&next);
                    return TextInputAction::Changed;
                }
                TextInputAction::Noop
            }

            // ── Word movement ────────────────────────────────────────────
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let line = &self.lines[self.cursor_row];
                let mut col = self.cursor_col.min(line.len());
                let bytes = line.as_bytes();
                while col > 0 && bytes[col - 1].is_ascii_whitespace() {
                    col -= 1;
                }
                while col > 0 && !bytes[col - 1].is_ascii_whitespace() {
                    col -= 1;
                }
                self.cursor_col = col;
                TextInputAction::Noop
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let line = &self.lines[self.cursor_row];
                let len = line.len();
                let mut col = self.cursor_col.min(len);
                let bytes = line.as_bytes();
                while col < len && !bytes[col].is_ascii_whitespace() {
                    col += 1;
                }
                while col < len && bytes[col].is_ascii_whitespace() {
                    col += 1;
                }
                self.cursor_col = col;
                TextInputAction::Noop
            }

            // ── Single-char movement ─────────────────────────────────────
            KeyCode::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.lines[self.cursor_row].len();
                }
                TextInputAction::Noop
            }
            KeyCode::Right => {
                let len = self.lines[self.cursor_row].len();
                if self.cursor_col < len {
                    self.cursor_col += 1;
                } else if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                }
                TextInputAction::Noop
            }
            KeyCode::Home => {
                self.cursor_col = 0;
                TextInputAction::Noop
            }
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_row].len();
                TextInputAction::Noop
            }

            // ── Vertical movement / history ──────────────────────────────
            KeyCode::Up => self.handle_up(),
            KeyCode::Down => self.handle_down(),

            // ── Enter / Shift+Enter ──────────────────────────────────────
            KeyCode::Enter => {
                let insert_newline = self.config.multiline
                    && (key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::SHIFT));
                if insert_newline {
                    let line = &self.lines[self.cursor_row];
                    let col = self.cursor_col.min(line.len());
                    let rest = line[col..].to_string();
                    self.lines[self.cursor_row].truncate(col);
                    self.cursor_row += 1;
                    self.lines.insert(self.cursor_row, rest);
                    self.cursor_col = 0;
                    TextInputAction::Changed
                } else {
                    TextInputAction::Submit
                }
            }

            KeyCode::Esc => {
                self.completions.clear();
                TextInputAction::Cancel
            }
            KeyCode::Tab => {
                self.completions.clear();
                TextInputAction::FocusNext
            }
            KeyCode::BackTab => {
                self.completions.clear();
                TextInputAction::FocusPrev
            }

            _ => TextInputAction::Noop,
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────

    fn accept_completion(&mut self) -> TextInputAction {
        let completion = self.completions[self.completion_idx].clone();
        let col = self.cursor_col.min(self.lines[self.cursor_row].len());
        let replace_start = self.completion_replace_start;
        let after = self.lines[self.cursor_row][col..].to_string();
        let new_line = format!(
            "{}{}{}",
            &self.lines[self.cursor_row][..replace_start],
            completion,
            after
        );
        let new_col = replace_start + completion.len();
        self.lines[self.cursor_row] = new_line;
        self.cursor_col = new_col;
        self.completions.clear();
        TextInputAction::Changed
    }

    fn handle_up(&mut self) -> TextInputAction {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let len = self.lines[self.cursor_row].len();
            self.cursor_col = self.cursor_col.min(len);
            TextInputAction::Noop
        } else if !self.history.is_empty() {
            // Browse history backward
            let new_idx = match self.history_idx {
                None => {
                    self.stash = Some(self.lines.clone());
                    self.history.len() - 1
                }
                Some(idx) => {
                    if idx > 0 {
                        idx - 1
                    } else {
                        return TextInputAction::Noop;
                    }
                }
            };
            self.history_idx = Some(new_idx);
            self.lines = vec![self.history[new_idx].clone()];
            self.cursor_row = 0;
            self.cursor_col = self.lines[0].len();
            TextInputAction::Changed
        } else {
            TextInputAction::Noop
        }
    }

    fn handle_down(&mut self) -> TextInputAction {
        let last_row = self.lines.len() - 1;
        if self.cursor_row < last_row {
            self.cursor_row += 1;
            let len = self.lines[self.cursor_row].len();
            self.cursor_col = self.cursor_col.min(len);
            TextInputAction::Noop
        } else if let Some(idx) = self.history_idx {
            // Browse history forward
            if idx + 1 < self.history.len() {
                let new_idx = idx + 1;
                self.history_idx = Some(new_idx);
                self.lines = vec![self.history[new_idx].clone()];
            } else {
                // Back to current edit
                self.history_idx = None;
                if let Some(stash) = self.stash.take() {
                    self.lines = stash;
                }
            }
            self.cursor_row = 0;
            self.cursor_col = self.lines[0].len();
            TextInputAction::Changed
        } else {
            TextInputAction::Noop
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Split a string into chunks of at most `width` characters.
fn wrap_str(s: &str, width: usize) -> Vec<&str> {
    if s.is_empty() || width == 0 {
        return vec![""];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let end = (start + width).min(s.len());
        chunks.push(&s[start..end]);
        start = end;
    }
    chunks
}

/// Render the text input content into an already-bordered inner area.
///
/// The caller is responsible for rendering the surrounding block/border.
pub fn render_inner(
    f: &mut Frame,
    area: Rect,
    state: &TextInputState,
    focused: bool,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let w = area.width as usize;
    let max_visual = area.height as usize;
    let text_style = Style::default().fg(theme.fg);
    let cursor_style = Style::default().bg(theme.accent).fg(theme.bg_surface);
    let ghost_style = Style::default().fg(theme.fg_dim);

    // Placeholder
    if !focused && state.is_empty() && !state.config.placeholder.is_empty() {
        let line = Line::from(Span::styled(
            state.config.placeholder.clone(),
            Style::default().fg(theme.fg_dim),
        ));
        f.render_widget(Paragraph::new(vec![line]), area);
        return;
    }

    let ghost = if focused { state.ghost_text() } else { None };

    let mut lines: Vec<Line> = Vec::new();

    for (row_idx, line_text) in state.lines.iter().enumerate() {
        if lines.len() >= max_visual {
            break;
        }

        let is_cursor_row = focused && row_idx == state.cursor_row;
        let chunks = if state.config.wrapping {
            wrap_str(line_text, w)
        } else {
            vec![line_text.as_str()]
        };

        if !is_cursor_row {
            for chunk in chunks {
                if lines.len() >= max_visual {
                    break;
                }
                lines.push(Line::from(Span::styled(chunk.to_string(), text_style)));
            }
            continue;
        }

        // Cursor line — render with cursor + ghost text
        let col = state.cursor_col.min(line_text.len());
        let cursor_chunk_idx = if state.config.wrapping && w > 0 {
            col / w
        } else {
            0
        };
        let cursor_visual_col = if state.config.wrapping && w > 0 {
            col % w
        } else {
            col
        };

        let num_chunks = chunks.len().max(1);
        for chunk_idx in 0..num_chunks {
            if lines.len() >= max_visual {
                break;
            }

            let chunk = if chunk_idx < chunks.len() {
                chunks[chunk_idx]
            } else {
                ""
            };

            if chunk_idx != cursor_chunk_idx {
                lines.push(Line::from(Span::styled(chunk.to_string(), text_style)));
                continue;
            }

            // This chunk contains the cursor
            let before = &chunk[..cursor_visual_col.min(chunk.len())];
            let mut spans = vec![Span::styled(before.to_string(), text_style)];

            if cursor_visual_col < chunk.len() {
                let cursor_char = &chunk[cursor_visual_col..cursor_visual_col + 1];
                let after = &chunk[cursor_visual_col + 1..];
                spans.push(Span::styled(cursor_char.to_string(), cursor_style));
                if let Some(g) = ghost {
                    let remaining = w.saturating_sub(cursor_visual_col + 1 + after.len());
                    if remaining > 0 {
                        let show = &g[..g.len().min(remaining)];
                        spans.push(Span::styled(show.to_string(), ghost_style));
                    }
                }
                spans.push(Span::styled(after.to_string(), text_style));
            } else {
                // Cursor at end of chunk
                if let Some(g) = ghost {
                    let remaining = w.saturating_sub(cursor_visual_col);
                    let truncated = &g[..g.len().min(remaining)];
                    if !truncated.is_empty() {
                        let first = &truncated[..1];
                        let rest = &truncated[1..];
                        spans.push(Span::styled(first.to_string(), cursor_style));
                        if !rest.is_empty() {
                            spans.push(Span::styled(rest.to_string(), ghost_style));
                        }
                    } else {
                        spans.push(Span::styled(" ", cursor_style));
                    }
                } else {
                    spans.push(Span::styled(" ", cursor_style));
                }
            }

            lines.push(Line::from(spans));
        }
    }

    f.render_widget(Paragraph::new(lines), area);
}
