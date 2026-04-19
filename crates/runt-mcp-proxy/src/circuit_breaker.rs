//! Circuit breaker for child process crash detection.
//!
//! Tracks recent crash timestamps and trips after too many in a short window,
//! preventing infinite restart loops.

use std::time::{Duration, Instant};

const MAX_CRASHES: usize = 5;
const WINDOW: Duration = Duration::from_secs(30);
/// After the breaker trips, wait this long before auto-resetting.
/// Gives the daemon time to fully restart after a binary upgrade.
const COOLDOWN: Duration = Duration::from_secs(60);

/// Tracks recent child crashes and decides whether to allow restart.
pub struct CircuitBreaker {
    recent_crashes: Vec<Instant>,
    /// When the breaker tripped — used for cooldown-then-retry.
    tripped_at: Option<Instant>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            recent_crashes: Vec::new(),
            tripped_at: None,
        }
    }

    /// Record a crash and return whether restart is allowed.
    ///
    /// Returns `false` (tripped) if there have been >= `MAX_CRASHES` in the
    /// last `WINDOW` seconds. After a cooldown period, the breaker auto-resets
    /// so the proxy can recover from daemon restarts without manual intervention.
    pub fn record_crash(&mut self) -> bool {
        let now = Instant::now();

        // Auto-reset after cooldown: if the breaker tripped long enough ago,
        // clear state and allow retry. This handles daemon restarts where the
        // child crashes rapidly during the restart window but should recover
        // once the new daemon is up.
        if let Some(tripped) = self.tripped_at {
            if now.duration_since(tripped) >= COOLDOWN {
                self.recent_crashes.clear();
                self.tripped_at = None;
            }
        }

        self.recent_crashes
            .retain(|t| now.duration_since(*t) < WINDOW);
        if self.recent_crashes.len() >= MAX_CRASHES {
            if self.tripped_at.is_none() {
                self.tripped_at = Some(now);
            }
            return false;
        }
        self.recent_crashes.push(now);
        true
    }

    /// Reset the circuit breaker (e.g., after a manual restart or file change).
    pub fn reset(&mut self) {
        self.recent_crashes.clear();
        self.tripped_at = None;
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_crashes_under_limit() {
        let mut cb = CircuitBreaker::new();
        for i in 0..MAX_CRASHES {
            assert!(cb.record_crash(), "crash {i} should be allowed");
        }
    }

    #[test]
    fn trips_at_limit() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash(), "crash beyond limit should trip breaker");
    }

    #[test]
    fn stays_tripped_on_repeated_calls() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        // Repeated calls should all return false
        for _ in 0..10 {
            assert!(!cb.record_crash());
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());
        cb.reset();
        assert!(cb.record_crash(), "should allow crashes after reset");
    }

    #[test]
    fn reset_allows_full_window_again() {
        let mut cb = CircuitBreaker::new();
        // Fill up and trip
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());

        // Reset and verify we get the full window again
        cb.reset();
        for i in 0..MAX_CRASHES {
            assert!(cb.record_crash(), "crash {i} after reset should be allowed");
        }
        assert!(
            !cb.record_crash(),
            "should trip again after hitting limit post-reset"
        );
    }

    #[test]
    fn new_and_default_are_equivalent() {
        let new = CircuitBreaker::new();
        let default = CircuitBreaker::default();
        // Both should allow the same number of crashes
        assert_eq!(new.recent_crashes.len(), default.recent_crashes.len());
    }

    #[test]
    fn first_crash_is_always_allowed() {
        let mut cb = CircuitBreaker::new();
        assert!(cb.record_crash(), "first crash should always be allowed");
    }

    #[test]
    fn exact_limit_boundary() {
        let mut cb = CircuitBreaker::new();
        // Record exactly MAX_CRASHES - 1
        for _ in 0..MAX_CRASHES - 1 {
            cb.record_crash();
        }
        // The last one within the limit should work
        assert!(cb.record_crash(), "crash at exactly the limit should work");
        // One more should fail
        assert!(!cb.record_crash(), "crash beyond limit should fail");
    }

    #[test]
    fn auto_resets_after_cooldown() {
        let mut cb = CircuitBreaker::new();
        // Trip the breaker
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash(), "should be tripped");
        assert!(cb.tripped_at.is_some(), "tripped_at should be set");

        // Simulate cooldown by backdating tripped_at
        cb.tripped_at = Some(Instant::now() - COOLDOWN - Duration::from_secs(1));

        // Next record_crash should auto-reset and allow
        assert!(cb.record_crash(), "should allow after cooldown auto-reset");
        assert!(cb.tripped_at.is_none(), "tripped_at should be cleared");
        assert_eq!(cb.recent_crashes.len(), 1, "should have fresh crash count");
    }

    #[test]
    fn stays_tripped_before_cooldown() {
        let mut cb = CircuitBreaker::new();
        // Trip the breaker
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());

        // Without cooldown elapsed, should stay tripped
        assert!(!cb.record_crash(), "should stay tripped before cooldown");
        assert!(cb.tripped_at.is_some());
    }

    #[test]
    fn manual_reset_clears_tripped_at() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());
        assert!(cb.tripped_at.is_some());

        cb.reset();
        assert!(cb.tripped_at.is_none(), "reset should clear tripped_at");
        assert!(cb.record_crash(), "should allow after manual reset");
    }

    #[test]
    fn repeated_trip_cooldown_cycles() {
        let mut cb = CircuitBreaker::new();

        for cycle in 0..3 {
            // Trip the breaker
            for _ in 0..MAX_CRASHES {
                cb.record_crash();
            }
            assert!(!cb.record_crash(), "cycle {cycle}: should be tripped");

            // Simulate cooldown
            cb.tripped_at = Some(Instant::now() - COOLDOWN - Duration::from_secs(1));

            // Should allow again
            assert!(
                cb.record_crash(),
                "cycle {cycle}: should allow after cooldown"
            );
            assert_eq!(
                cb.recent_crashes.len(),
                1,
                "cycle {cycle}: should have fresh crash count"
            );
        }
    }
}
