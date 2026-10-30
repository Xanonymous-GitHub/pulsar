//! A byte budget for what waits in a queue.
//!
//! The MQTT task charges every incoming message before it queues it, and the dispatcher thread
//! releases the same amount when it takes the message off. A message that does not fit is dropped,
//! so a slow handler cannot use up the heap. No crate types, so it can be tested on a computer
//! (`rustc --test src/mqtt/budget.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};

/// How many bytes may wait at the same time.
pub struct Budget {
    used: AtomicUsize,
    limit: usize,
}

impl Budget {
    /// A budget of `limit` bytes.
    pub const fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    /// Reserves `cost` bytes. Returns `false`, and reserves nothing, if they do not fit.
    ///
    /// Only one thread may charge (the MQTT task): the check and the reservation are not one
    /// atomic step.
    pub fn try_charge(&self, cost: usize) -> bool {
        let before = self.used.fetch_add(cost, Ordering::Relaxed);
        let fits = before.saturating_add(cost) <= self.limit;
        if !fits {
            self.used.fetch_sub(cost, Ordering::Relaxed);
        }
        fits
    }

    /// Gives back `cost` bytes that [`Budget::try_charge`] reserved.
    pub fn release(&self, cost: usize) {
        self.used.fetch_sub(cost, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_up_to_the_limit() {
        let budget = Budget::new(100);
        assert!(budget.try_charge(60));
        assert!(budget.try_charge(40));
        assert!(!budget.try_charge(1));
    }

    #[test]
    fn a_refused_charge_reserves_nothing() {
        let budget = Budget::new(100);
        assert!(budget.try_charge(90));
        assert!(!budget.try_charge(20));
        assert!(budget.try_charge(10));
    }

    #[test]
    fn released_bytes_can_be_used_again() {
        let budget = Budget::new(100);
        assert!(budget.try_charge(100));
        assert!(!budget.try_charge(1));
        budget.release(100);
        assert!(budget.try_charge(100));
    }

    #[test]
    fn many_small_charges_are_bounded() {
        // A message with no payload still costs its topic and bookkeeping.
        let budget = Budget::new(12 * 1024);
        let cost = 1018 + 64;
        let accepted = (0..1000).filter(|_| budget.try_charge(cost)).count();
        assert_eq!(accepted, 12 * 1024 / cost);
    }

    #[test]
    fn a_huge_charge_does_not_overflow() {
        let budget = Budget::new(100);
        assert!(budget.try_charge(10));
        assert!(!budget.try_charge(usize::MAX));
        assert!(budget.try_charge(90));
    }
}
