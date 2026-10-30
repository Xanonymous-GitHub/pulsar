//! A request that is carried out only after something confirms it, within a time limit.
//!
//! The restart command uses it. The broker replays a retained command after every subscribe, so
//! the board must not reboot before the command is gone from the broker. It clears the command,
//! and the broker's echo of that clear is the confirmation. No crate types, so it can be tested on
//! a computer (`rustc --test src/telemetry/confirmation.rs`).

use std::time::{Duration, Instant};

/// At most one request that waits for its confirmation.
pub struct Confirmation {
    limit: Duration,
    asked: Option<Instant>,
}

impl Confirmation {
    /// A request is dropped when its confirmation takes longer than `limit`.
    pub const fn new(limit: Duration) -> Self {
        Self { limit, asked: None }
    }

    /// Starts waiting for a confirmation. Returns `false` if an earlier request is still waiting
    /// (within its limit), which then stays as it is.
    pub fn ask(&mut self, now: Instant) -> bool {
        if self.asked.is_some_and(|asked| !self.expired(asked, now)) {
            return false;
        }
        self.asked = Some(now);
        true
    }

    /// Ends the wait. Returns `true` if a request was waiting and the confirmation came in time.
    pub fn confirm(&mut self, now: Instant) -> bool {
        self.asked
            .take()
            .is_some_and(|asked| !self.expired(asked, now))
    }

    /// Gives up the waiting request, for example because it could not be sent.
    pub fn cancel(&mut self) {
        self.asked = None;
    }

    /// Drops a request whose limit has passed. Returns `true` if there was one, so the caller can
    /// say that the confirmation never came.
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.asked {
            Some(asked) if self.expired(asked, now) => {
                self.asked = None;
                true
            }
            _ => false,
        }
    }

    fn expired(&self, asked: Instant, now: Instant) -> bool {
        now.saturating_duration_since(asked) > self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(10);

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn a_confirmation_without_a_request_is_nothing() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(!confirmation.confirm(start));
    }

    #[test]
    fn a_confirmation_in_time_counts_once() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        assert!(confirmation.confirm(at(start, 3)));
        assert!(!confirmation.confirm(at(start, 4)), "one request, one go");
    }

    #[test]
    fn a_late_confirmation_does_not_count_and_ends_the_wait() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        assert!(!confirmation.confirm(at(start, 11)));
        assert!(!confirmation.confirm(at(start, 12)));
    }

    #[test]
    fn a_confirmation_at_the_limit_counts() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        assert!(confirmation.confirm(at(start, 10)));
    }

    #[test]
    fn a_second_request_waits_for_the_first() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        assert!(!confirmation.ask(at(start, 5)));
        // The first request keeps its own start time, so it still expires on schedule.
        assert!(!confirmation.confirm(at(start, 11)));
    }

    #[test]
    fn an_expired_request_makes_room_for_a_new_one() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        assert!(confirmation.ask(at(start, 11)));
        assert!(confirmation.confirm(at(start, 12)));
    }

    #[test]
    fn cancel_forgets_the_request() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(confirmation.ask(start));
        confirmation.cancel();
        assert!(!confirmation.confirm(at(start, 1)));
        assert!(confirmation.ask(at(start, 1)));
    }

    #[test]
    fn expire_reports_a_lost_request_once() {
        let start = Instant::now();
        let mut confirmation = Confirmation::new(LIMIT);
        assert!(!confirmation.expire(start), "nothing was asked");
        assert!(confirmation.ask(start));
        assert!(!confirmation.expire(at(start, 5)), "still within the limit");
        assert!(confirmation.expire(at(start, 11)));
        assert!(!confirmation.expire(at(start, 12)));
    }
}
