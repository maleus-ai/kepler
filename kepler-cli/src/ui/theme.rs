use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

#[allow(dead_code)]
pub struct Theme {
    // Base — Catppuccin Mocha tiers (darkest → lightest)
    pub bg: Color,              // crust — darkest, gaps between panels
    pub bg_mantle: Color,       // mantle — slightly lighter, outer frame
    pub bg_surface: Color,      // base — panel interiors
    pub bg_overlay: Color,      // surface0 — overlays, popups
    pub bg_highlight: Color,    // surface1 — selection highlight

    // Text
    pub fg: Color,
    pub fg_dim: Color,
    pub fg_bright: Color,
    pub subtext: Color,

    // Accents
    pub accent: Color,
    pub accent_dim: Color,
    pub mauve: Color,
    pub sapphire: Color,
    pub teal: Color,
    pub pink: Color,

    // Semantic
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub peach: Color,

    // Structure
    pub border: Color,          // subtle border (surface1)
    pub border_focused: Color,  // accent blue for focused panels
    pub selection_bg: Color,
    pub selection_fg: Color,

    // Chart (10 colors for multi-service support)
    pub chart_colors: [Color; 10],
}

#[allow(dead_code)]
impl Theme {
    pub fn default_theme() -> Self {
        Self {
            // Base — proper Catppuccin Mocha tiers for depth
            bg: Color::Rgb(17, 17, 27),              // crust — darkest, visible gaps
            bg_mantle: Color::Rgb(24, 24, 37),       // mantle — outer frame bg
            bg_surface: Color::Rgb(30, 30, 46),      // base — panel interiors
            bg_overlay: Color::Rgb(49, 50, 68),      // surface0 — popups
            bg_highlight: Color::Rgb(69, 71, 90),    // surface1 — selection

            // Text
            fg: Color::Rgb(205, 214, 244),           // text
            fg_dim: Color::Rgb(108, 112, 134),       // overlay1
            fg_bright: Color::Rgb(205, 214, 244),    // text
            subtext: Color::Rgb(166, 173, 200),      // subtext0

            // Accents
            accent: Color::Rgb(137, 180, 250),       // blue
            accent_dim: Color::Rgb(180, 190, 254),   // lavender
            mauve: Color::Rgb(203, 166, 247),
            sapphire: Color::Rgb(116, 199, 236),
            teal: Color::Rgb(148, 226, 213),
            pink: Color::Rgb(245, 194, 231),

            // Semantic
            success: Color::Rgb(166, 227, 161),      // green
            error: Color::Rgb(243, 139, 168),        // red
            warning: Color::Rgb(249, 226, 175),      // yellow
            peach: Color::Rgb(250, 179, 135),        // orange/peach

            // Structure
            border: Color::Rgb(69, 71, 90),          // surface1
            border_focused: Color::Rgb(137, 180, 250), // blue — highlighted borders
            selection_bg: Color::Rgb(69, 71, 90),    // surface1
            selection_fg: Color::Rgb(205, 214, 244), // text

            // Chart: blue, mauve, teal, peach, pink, sapphire, green, yellow, red, lavender
            chart_colors: [
                Color::Rgb(137, 180, 250),
                Color::Rgb(203, 166, 247),
                Color::Rgb(148, 226, 213),
                Color::Rgb(250, 179, 135),
                Color::Rgb(245, 194, 231),
                Color::Rgb(116, 199, 236),
                Color::Rgb(166, 227, 161),
                Color::Rgb(249, 226, 175),
                Color::Rgb(243, 139, 168),
                Color::Rgb(180, 190, 254),
            ],
        }
    }

    /// Unfocused panel — border blends into panel bg (invisible border).
    pub fn block<'a>(&self, title: &'a str) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(self.border))
            .title(title)
            .title_style(Style::default().fg(self.fg_dim))
            .style(Style::default().bg(self.bg_surface))
    }

    /// Focused panel — border highlighted with accent color.
    pub fn block_focused<'a>(&self, title: &'a str) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(self.border_focused))
            .title(title)
            .title_style(Style::default().fg(self.fg_bright))
            .style(Style::default().bg(self.bg_surface))
    }

    pub fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.bg_highlight)
            .fg(self.selection_fg)
    }

    pub fn header_style(&self) -> Style {
        Style::default()
            .fg(self.fg_bright)
            .add_modifier(Modifier::BOLD)
    }

    pub fn dim_style(&self) -> Style {
        Style::default().fg(self.fg_dim)
    }

    /// Map a service status string to its theme color.
    pub fn status_color(&self, status: &str) -> Color {
        match status {
            "running" | "healthy" => self.success,
            "starting" | "waiting" | "restarting" | "pending_restart" => self.warning,
            "stopped" | "exited" => self.fg_dim,
            "failed" | "killed" => self.error,
            "unhealthy" => self.peach,
            "pending" => self.subtext,
            "skipped" => self.fg_dim,
            _ => self.fg_dim,
        }
    }

    /// Map a service status string to a Unicode icon.
    pub fn status_icon(&self, status: &str) -> &'static str {
        match status {
            "running" => "\u{25cf}",   // ●
            "healthy" => "\u{2713}",   // ✓
            "starting" | "waiting" | "restarting" | "pending_restart" => "\u{25d0}", // ◐
            "stopped" | "exited" => "\u{25cb}",   // ○
            "failed" | "killed" => "\u{2717}",    // ✗
            "unhealthy" => "\u{26a0}",  // ⚠
            "pending" => "\u{25cc}",    // ◌
            "skipped" => "\u{2298}",    // ⊘
            _ => "\u{25cb}",            // ○
        }
    }
}
