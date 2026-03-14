pub mod event;
pub mod pages;
pub mod terminal;
pub mod theme;
pub mod widgets;

/// Tab-based navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Logs,
    Detail,
}

/// Overlay state — at most one overlay visible at a time
#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Help,
    ContextMenu {
        service: String,
        x: u16,
        y: u16,
        selected: usize,
    },
}

/// Internal app events
#[allow(dead_code)]
pub enum AppEvent {
    Key(crossterm::event::KeyEvent),
    Mouse(crossterm::event::MouseEvent),
    Tick,
    Resize(u16, u16),
}
