//! Retry delays. Pure, with the random source passed in so tests are deterministic.

use std::time::Duration;

/// Exponential backoff with full jitter: the delay is a random fraction of `min(cap, base * 2^n)`.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    #[must_use]
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            attempt: 0,
        }
    }

    /// The socket link: 1 second base, 60 second cap.
    #[must_use]
    pub fn socket() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(60))
    }

    /// The next delay for a jitter in `[0, 1]`. Never exceeds the cap.
    pub fn next_delay(&mut self, jitter: f64) -> Duration {
        let exp = self
            .base
            .saturating_mul(2_u32.saturating_pow(self.attempt.min(30)));
        let ceiling = exp.min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        ceiling.mul_f64(jitter.clamp(0.0, 1.0))
    }

    /// Forget past failures (call only after a connection has streamed healthily).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempt
    }
}

/// A cheap jitter in `[0, 1)` from the clock; good enough for spreading reconnects.
#[must_use]
pub fn clock_jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_never_exceed_the_cap() {
        let mut b = Backoff::socket();
        for _ in 0..200 {
            assert!(b.next_delay(1.0) <= Duration::from_secs(60));
        }
    }

    #[test]
    fn the_ceiling_doubles_up_to_the_cap() {
        let mut b = Backoff::socket();
        let secs: Vec<u64> = (0..8).map(|_| b.next_delay(1.0).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn full_jitter_can_shorten_the_delay_but_not_lengthen_it() {
        let mut b = Backoff::socket();
        assert_eq!(b.next_delay(0.0), Duration::ZERO);
        assert!(b.next_delay(0.5) <= Duration::from_secs(1));
    }

    #[test]
    fn a_flapping_connection_keeps_growing_until_an_explicit_reset() {
        let mut b = Backoff::socket();
        for _ in 0..4 {
            b.next_delay(1.0);
        }
        // a reconnect that dropped again within five minutes does not call reset()
        assert_eq!(b.next_delay(1.0), Duration::from_secs(16));
        b.reset();
        assert_eq!(b.next_delay(1.0), Duration::from_secs(1));
    }
}
