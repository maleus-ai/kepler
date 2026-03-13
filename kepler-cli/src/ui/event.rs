use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event};
use tokio::sync::mpsc;

use super::AppEvent;

/// Spawn a background task that reads terminal events and sends them through a channel.
///
/// Uses crossterm's blocking poll with short timeouts so the stop flag can be checked
/// frequently. Returns the receiver end of the channel.
pub fn spawn_event_reader(stop_flag: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<AppEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::task::spawn_blocking(move || {
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            // Short poll (50ms) so we can check the stop flag frequently
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => {
                        if tx.send(AppEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Mouse(mouse)) => {
                        if tx.send(AppEvent::Mouse(mouse)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(w, h)) => {
                        if tx.send(AppEvent::Resize(w, h)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {} // No event within 50ms, loop to check stop flag
                Err(_) => break,
            }
        }
    });

    rx
}
