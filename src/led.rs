//! The blinking user LED.

use std::io;
use std::thread::{self, JoinHandle};

use crate::board::Led;
use crate::config::{LED_PERIOD, LED_STACK_BYTES};

/// Starts a thread that toggles `led` forever.
///
/// # Errors
///
/// Fails if the thread cannot be created.
pub fn spawn(mut led: Led) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("led".into())
        .stack_size(LED_STACK_BYTES)
        .spawn(move || {
            loop {
                if let Err(error) = led.toggle() {
                    log::error!("toggle failed: {error}");
                    return;
                }
                thread::sleep(LED_PERIOD);
            }
        })
}
