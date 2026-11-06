//! Nonces. The exchange keeps the 100 highest per signer and needs each new one above the smallest
//! of those, unused, and inside `(T - 2 days, T + 1 day)` of block time.

/// The exchange's forward window, in milliseconds.
pub const NONCE_AHEAD_MS_MAX: u64 = 86_400_000;

const _: () = assert!(NONCE_AHEAD_MS_MAX == 24 * 60 * 60 * 1000);

/// A strictly increasing source of nonces that stays near wall-clock time.
///
/// `next` is `max(previous + 1, now)`. Sending faster than 1,000 per second borrows from the
/// future, and a clock stepping backward cannot repeat a nonce: this is the fast-forwarded atomic
/// counter the docs recommend, without the atomic, since the engine owns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NonceSource {
    previous: u64,
}

/// The clock is so far behind the nonces already used that the exchange would refuse the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NonceTooFarAhead;

impl NonceSource {
    /// Starts above `floor`, which a process that restarted can take from anything durable. Time
    /// alone covers a restart unless the clock moved back.
    #[must_use]
    pub const fn new(floor: u64) -> NonceSource {
        assert!(floor < u64::MAX);
        NonceSource { previous: floor }
    }

    pub fn next(&mut self, now_ms: u64) -> Result<u64, NonceTooFarAhead> {
        assert!(self.previous < u64::MAX);
        let nonce = now_ms.max(self.previous + 1);
        assert!(nonce >= now_ms);
        if nonce - now_ms >= NONCE_AHEAD_MS_MAX {
            return Err(NonceTooFarAhead);
        }
        assert!(nonce > self.previous);
        self.previous = nonce;
        Ok(nonce)
    }

    #[must_use]
    pub const fn previous(&self) -> u64 {
        assert!(self.previous < u64::MAX);
        self.previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strictly_increases_whatever_the_clock_does() {
        let mut source = NonceSource::new(0);
        assert_eq!(source.next(1_000), Ok(1_000));
        assert_eq!(source.next(1_000), Ok(1_001));
        assert_eq!(source.next(999), Ok(1_002));
        assert_eq!(source.next(5_000), Ok(5_000));
        assert_eq!(source.next(1), Ok(5_001));
    }

    #[test]
    fn a_restart_above_its_floor_never_repeats() {
        let mut source = NonceSource::new(1_000);
        assert_eq!(source.next(500), Ok(1_001));
    }

    #[test]
    fn refuses_rather_than_sign_past_the_window() {
        let mut source = NonceSource::new(NONCE_AHEAD_MS_MAX + 1_000);
        assert_eq!(source.next(1_000), Err(NonceTooFarAhead));
        assert_eq!(source.previous(), NONCE_AHEAD_MS_MAX + 1_000);
        assert!(source.next(2_002).is_ok());
    }
}
