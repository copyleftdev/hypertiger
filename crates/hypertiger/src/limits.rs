//! The exchange's published limits, and the integer token bucket that keeps a client under them.

/// REST weight allowed per IP per minute.
pub const IP_WEIGHT_PER_MINUTE: u32 = 1200;
pub const WS_CONNECTIONS_MAX: u32 = 10;
pub const WS_CONNECTIONS_NEW_PER_MINUTE: u32 = 30;
pub const WS_SUBSCRIPTIONS_MAX: u32 = 1000;
pub const WS_USERS_MAX: u32 = 10;
pub const WS_MESSAGES_PER_MINUTE: u32 = 2000;
pub const WS_POSTS_INFLIGHT_MAX: u32 = 100;
/// The server closes a connection it has sent nothing on for this long.
pub const WS_IDLE_MS_MAX: u32 = 60_000;
/// A ping interval that leaves room for one lost ping before the server gives up.
pub const WS_PING_INTERVAL_MS: u32 = 25_000;

const _: () = assert!(WS_PING_INTERVAL_MS * 2 < WS_IDLE_MS_MAX);

const MS_PER_MINUTE: u64 = 60_000;

/// An action's weight: `1 + floor(batch_length / 40)`.
#[must_use]
pub const fn exchange_weight(batch_length: u32) -> u32 {
    1 + batch_length / 40
}

const _: () = assert!(exchange_weight(0) == 1);
const _: () = assert!(exchange_weight(39) == 1);
const _: () = assert!(exchange_weight(40) == 2);
const _: () = assert!(exchange_weight(79) == 2);

/// A token bucket in integer arithmetic, with time passed in.
///
/// One unit of `credits` is one weight-millisecond: a bucket of rate `r` per minute gains `r` per
/// millisecond and spends 60,000 per weight, so there is no rounding to accumulate.
///
/// `burst` matters more than it looks. A bucket holding `b` can spend `b` at once and then `r` over
/// the next minute, so `b + r` is what any sliding minute can reach. Keeping under a limit `L`
/// takes `b + r <= L`, not `r <= L`; `TokenBucket::new` leaves that to the caller's constants and
/// the simulator's oracle checks the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenBucket {
    credits: u64,
    credits_max: u64,
    rate_per_minute: u64,
    time_ms: u64,
    /// Weight charged after the fact, repaid from refills before anything else may spend.
    debt: u64,
}

impl TokenBucket {
    #[must_use]
    pub fn new(rate_per_minute: u32, burst: u32, time_ms: u64) -> TokenBucket {
        assert!(rate_per_minute > 0);
        assert!(burst > 0);
        let credits_max = u64::from(burst) * MS_PER_MINUTE;
        TokenBucket {
            credits: credits_max,
            credits_max,
            rate_per_minute: u64::from(rate_per_minute),
            time_ms,
            debt: 0,
        }
    }

    /// A bucket that starts with nothing. A process that restarts cannot know what its previous
    /// life spent, and the exchange's limit is per IP and outlives it, so a fresh full bucket would
    /// let a crash loop pass the limit. Starting empty treats every start as following a spent
    /// bucket.
    #[must_use]
    pub fn new_empty(rate_per_minute: u32, burst: u32, time_ms: u64) -> TokenBucket {
        let mut bucket = TokenBucket::new(rate_per_minute, burst, time_ms);
        bucket.credits = 0;
        assert!(bucket.credits_max > 0);
        bucket
    }

    /// Records weight only known once a reply arrives. The docs add weight per item returned on
    /// some requests, and this is how that gets paid for.
    pub fn charge(&mut self, time_ms: u64, weight: u32) {
        self.refill(time_ms);
        let owed = u64::from(weight) * MS_PER_MINUTE;
        let paid = owed.min(self.credits);
        self.credits -= paid;
        self.debt = self.debt.saturating_add(owed - paid);
        assert!(self.credits <= self.credits_max);
        assert!(self.debt == 0 || self.credits == 0, "nothing is held while something is owed");
    }

    fn refill(&mut self, time_ms: u64) {
        // A clock that steps backward adds nothing and takes nothing away.
        if time_ms > self.time_ms {
            let gained = (time_ms - self.time_ms).saturating_mul(self.rate_per_minute);
            let repaid = gained.min(self.debt);
            self.debt -= repaid;
            self.credits = self.credits.saturating_add(gained - repaid).min(self.credits_max);
            self.time_ms = time_ms;
        }
        assert!(self.credits <= self.credits_max);
        assert!(self.debt == 0 || self.credits == 0, "nothing is held while something is owed");
    }

    /// Spends `weight`, provided that leaves at least `reserve` weight behind. A reserve is how
    /// cancels stay possible when orders have used up the budget.
    pub fn take(&mut self, time_ms: u64, weight: u32, reserve: u32) -> bool {
        assert!(weight > 0);
        self.refill(time_ms);
        let wanted = u64::from(weight).saturating_add(u64::from(reserve)) * MS_PER_MINUTE;
        if self.credits >= wanted {
            self.credits -= u64::from(weight) * MS_PER_MINUTE;
            true
        } else {
            false
        }
    }

    /// Weight available now, rounded down.
    #[must_use]
    pub fn available(&mut self, time_ms: u64) -> u32 {
        self.refill(time_ms);
        let whole = self.credits / MS_PER_MINUTE;
        assert!(self.debt == 0 || whole == 0);
        assert!(u32::try_from(whole).is_ok());
        whole as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spends_down_and_refills_exactly() {
        let mut bucket = TokenBucket::new(60, 10, 0);
        assert_eq!(bucket.available(0), 10);
        assert!(bucket.take(0, 10, 0));
        assert!(!bucket.take(0, 1, 0));
        // 60 per minute is one per second, to the millisecond.
        assert!(!bucket.take(999, 1, 0));
        assert!(bucket.take(1000, 1, 0));
        assert!(!bucket.take(1000, 1, 0));
    }

    #[test]
    fn never_holds_more_than_the_burst() {
        let mut bucket = TokenBucket::new(60, 10, 0);
        assert_eq!(bucket.available(u64::MAX), 10);
        assert!(bucket.take(u64::MAX, 10, 0));
        assert!(!bucket.take(u64::MAX, 1, 0));
    }

    #[test]
    fn a_reserve_is_left_behind_and_can_itself_be_spent() {
        let mut bucket = TokenBucket::new(60, 10, 0);
        assert!(bucket.take(0, 5, 5));
        assert!(!bucket.take(0, 1, 5));
        assert!(bucket.take(0, 5, 0));
        assert_eq!(bucket.available(0), 0);
    }

    #[test]
    fn a_charge_past_the_balance_is_repaid_before_anything_is_spent() {
        let mut bucket = TokenBucket::new(60, 10, 0);
        bucket.charge(0, 15);
        assert_eq!(bucket.available(0), 0);
        assert_eq!(bucket.available(4_999), 0, "one second per weight, and five are owed");
        assert_eq!(bucket.available(5_000), 0);
        assert_eq!(bucket.available(6_000), 1);
    }

    #[test]
    fn an_empty_start_earns_its_budget() {
        let mut bucket = TokenBucket::new_empty(60, 10, 0);
        assert_eq!(bucket.available(0), 0);
        assert!(!bucket.take(0, 1, 0));
        assert!(bucket.take(1_000, 1, 0));
    }

    #[test]
    fn a_clock_stepping_backward_changes_nothing() {
        let mut bucket = TokenBucket::new(60, 10, 5_000);
        assert!(bucket.take(5_000, 10, 0));
        assert_eq!(bucket.available(1_000), 0);
        assert_eq!(bucket.available(5_000), 0);
        assert_eq!(bucket.available(6_000), 1);
    }
}
