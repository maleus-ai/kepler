use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Guard that restores the terminal on drop — ensures cleanup even on early
/// returns, `?` propagation, or signal-triggered exits.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = teardown_io();
    }
}

pub fn setup(signal_flag: Arc<AtomicBool>) -> io::Result<(Tui, TerminalGuard)> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Enable keyboard enhancement so modifiers like Shift+Enter are reported
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    );

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    // Install a panic hook that restores the terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = teardown_io();
        original_hook(panic_info);
    }));

    // Install signal handler for SIGTERM / SIGINT so that the guard's Drop
    // runs and the terminal is restored even when the process is killed.
    let flag = signal_flag;
    let _ = ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
        // Also restore terminal immediately in case the main loop is blocked
        let _ = teardown_io();
    });

    Ok((terminal, TerminalGuard))
}

pub fn teardown(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn teardown_io() -> io::Result<()> {
    disable_raw_mode()?;
    let mut stdout = io::stdout();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}
