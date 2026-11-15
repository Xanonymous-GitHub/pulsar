//! One TLS handshake at a time.
//!
//! A TLS handshake needs 30 to 40 KB of heap for a moment (the certificate chain, the public-key
//! arithmetic). With Wi-Fi, Bluetooth and the screen running the board has about 80 KB free, so
//! two handshakes at once run it dry: one of them fails (`-0x008D`, "PSA out of memory"), and
//! anything else that allocates at that moment fails with it. This was the rule at every boot:
//! the weather request and the MQTT connection both start when Wi-Fi gets its address.
//!
//! So whoever is about to open a TLS connection takes a turn first:
//!
//! ```ignore
//! let _turn = handshake::take_turn(config::TLS_HANDSHAKE_PATIENCE);
//! // connect; the turn ends when `_turn` is dropped
//! ```
//!
//! [`take_turn`] waits while another handshake is in progress, but not for ever: after the
//! patience given it goes ahead anyway, so one stuck connection cannot hold up everything else.
//!
//! Code that cannot wait (the MQTT task announces a connection attempt from a callback that must
//! not block) calls [`enter`] and [`leave`] instead. It does not wait for others, but others wait
//! for it. [`enter_in_turn`] is [`take_turn`] for a handshake that somebody else ends with
//! [`leave`].
//!
//! This file imports nothing from the crate, so it is tested on the host:
//!
//! ```sh
//! rustc --edition 2024 --test src/handshake.rs -o /tmp/t && /tmp/t
//! ```

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How often a waiting [`take_turn`] looks again.
const POLL_PERIOD: Duration = Duration::from_millis(50);

/// Counts the handshakes in progress.
#[derive(Debug, Default)]
struct Gate {
    in_progress: AtomicU32,
}

impl Gate {
    const fn new() -> Self {
        Self {
            in_progress: AtomicU32::new(0),
        }
    }

    fn enter(&self) {
        self.in_progress.fetch_add(1, Ordering::AcqRel);
    }

    fn leave(&self) {
        // Never below zero: a `leave` too many must not block everybody else for ever.
        let _ = self
            .in_progress
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        self.in_progress.load(Ordering::Acquire) == 0
    }

    /// Enters as soon as nobody else is inside, or after `patience` at the latest.
    fn enter_in_turn(&self, patience: Duration) {
        let deadline = Instant::now() + patience;
        while self
            .in_progress
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            if Instant::now() >= deadline {
                self.enter();
                return;
            }
            thread::sleep(POLL_PERIOD);
        }
    }
}

/// The gate of the whole firmware.
static GATE: Gate = Gate::new();

/// A turn at the handshake gate. It ends when this is dropped.
#[derive(Debug)]
#[must_use = "the turn ends as soon as this is dropped"]
pub struct Turn(());

impl Drop for Turn {
    fn drop(&mut self) {
        GATE.leave();
    }
}

/// Waits until no other TLS handshake is in progress, for at most `patience`, and then starts a
/// turn (also if the wait ran out). Keep the [`Turn`] until the connection is set up.
pub fn take_turn(patience: Duration) -> Turn {
    GATE.enter_in_turn(patience);
    Turn(())
}

/// Like [`take_turn`], for a handshake that is ended with [`leave`] instead of by dropping a
/// [`Turn`]: it happens on another task, which reports when it is over.
pub fn enter_in_turn(patience: Duration) {
    GATE.enter_in_turn(patience);
}

/// Announces a handshake without waiting for a turn. Every call needs one [`leave`].
pub fn enter() {
    GATE.enter();
}

/// The handshake announced with [`enter`] is over (it worked or it failed).
pub fn leave() {
    GATE.leave();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const LONG: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(120);

    #[test]
    fn a_free_gate_is_entered_at_once() {
        let gate = Gate::new();
        let start = Instant::now();
        gate.enter_in_turn(LONG);
        assert!(start.elapsed() < POLL_PERIOD);
        assert!(!gate.is_idle());
        gate.leave();
        assert!(gate.is_idle());
    }

    #[test]
    fn the_second_one_waits_for_the_first() {
        let gate = Arc::new(Gate::new());
        gate.enter_in_turn(LONG);
        let other = Arc::clone(&gate);
        let waiter = thread::spawn(move || {
            let start = Instant::now();
            other.enter_in_turn(LONG);
            start.elapsed()
        });
        thread::sleep(SHORT);
        gate.leave();
        let waited = waiter.join().unwrap();
        assert!(
            waited >= SHORT,
            "entered after {waited:?}, while the first was inside"
        );
        assert!(waited < LONG);
        assert!(!gate.is_idle(), "the second one is inside now");
    }

    #[test]
    fn patience_runs_out_and_it_goes_ahead() {
        let gate = Gate::new();
        gate.enter();
        let start = Instant::now();
        gate.enter_in_turn(SHORT);
        assert!(start.elapsed() >= SHORT);
        // Both are inside; the gate is free again only when both have left.
        gate.leave();
        assert!(!gate.is_idle());
        gate.leave();
        assert!(gate.is_idle());
    }

    #[test]
    fn enter_does_not_wait_but_others_wait_for_it() {
        let gate = Gate::new();
        gate.enter_in_turn(LONG);
        let start = Instant::now();
        gate.enter();
        assert!(start.elapsed() < POLL_PERIOD);
        // A third one waits for both of them (here: until its patience runs out).
        let start = Instant::now();
        gate.enter_in_turn(SHORT);
        assert!(start.elapsed() >= SHORT);
        gate.leave();
        gate.leave();
        gate.leave();
        assert!(gate.is_idle());
    }

    #[test]
    fn a_leave_too_many_does_no_harm() {
        let gate = Gate::new();
        gate.leave();
        gate.leave();
        assert!(gate.is_idle());
        gate.enter();
        assert!(!gate.is_idle(), "the count did not go below zero");
        gate.leave();
        assert!(gate.is_idle());
    }

    #[test]
    fn only_one_of_many_is_inside_at_a_time() {
        let gate = Arc::new(Gate::new());
        let inside = Arc::new(AtomicU32::new(0));
        let most = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (gate, inside, most) =
                    (Arc::clone(&gate), Arc::clone(&inside), Arc::clone(&most));
                thread::spawn(move || {
                    gate.enter_in_turn(LONG);
                    let now = inside.fetch_add(1, Ordering::AcqRel) + 1;
                    most.fetch_max(now, Ordering::AcqRel);
                    thread::sleep(Duration::from_millis(5));
                    inside.fetch_sub(1, Ordering::AcqRel);
                    gate.leave();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(most.load(Ordering::Acquire), 1);
        assert!(gate.is_idle());
    }
}
