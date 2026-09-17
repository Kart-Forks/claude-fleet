//! Terminal input, read by a thread of its own.
//!
//! The console's input queue is a fixed-size ring: records that land in it
//! while nobody is reading are dropped, silently and mid-word. The event loop
//! only got to read between renders, so a paste — which arrives as a flood of
//! key records, since crossterm produces `Event::Paste` on unix only — lost
//! whole runs of characters to every redraw it overlapped.
//!
//! So reading happens here instead, in a thread that does nothing else and sits
//! blocked in `read` the rest of the time. Rendering can take as long as it
//! likes; the queue is still being drained.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use crossterm::event::{self, Event};

pub struct Input {
    rx: Receiver<Event>,
}

impl Input {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
        Self { rx }
    }

    /// The next event, or `None` if none arrived within `timeout`.
    pub fn next_within(&self, timeout: Duration) -> Option<Event> {
        match self.rx.recv_timeout(timeout) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            // The reader is gone, so no event is ever coming. Wait out the
            // timeout anyway: returning at once would spin the event loop.
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                None
            }
        }
    }
}
