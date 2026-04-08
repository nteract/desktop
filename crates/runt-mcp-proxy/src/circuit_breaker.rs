//! Circuit breaker for child process crash detection.
//!
//! Tracks recent crash timestamps and trips after too many in a short window,
//! preventing infinite restart loops.

use std::time::{Duration, Instant};

const MAX_CRASHES: usize = 5;
const WINDOW: Duration = Duration::from_secs(30);

/// Tracks recent child crashes and decides whether to allow restart.
pub struct CircuitBreaker {
    recent_crashes: Vec<Instant>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            recent_crashes: Vec::new(),
        }
    }

    /// Record a crash and return whether restart is allowed.
    ///
    /// Returns `false` (tripped) if there have been >= `MAX_CRASHES` in the
    /// last `WINDOW` seconds.
    pub fn record_crash(&mut self) -> bool {
        let now = Instant::now();
        self.recent_crashes
            .retain(|t| now.duration_since(*t) < WINDOW);
        if self.recent_crashes.len() >= MAX_CRASHES {
            return false;
        }
        self.recent_crashes.push(now);
        true
    }

    /// Reset the circuit breaker (e.g., after a manual restart or file change).
    pub fn reset(&mut self) {
        self.recent_crashes.clear();
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn allows_crashes_under_limit() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            assert!(cb.record_crash());
        }
    }

    #[test]
    fn trips_at_limit() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());
    }

    #[test]
    fn reset_clears_state() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..MAX_CRASHES {
            cb.record_crash();
        }
        assert!(!cb.record_crash());
        cb.reset();
        assert!(cb.record_crash());
    }
}
