//! A fixed-size L2 book, replaced whole on each snapshot.
//!
//! The `l2Book` feed sends snapshots, not diffs, at most every half second and at most 20 levels a
//! side. So a book is two fixed arrays and needs no heap, no tree and no sequencing logic. It is
//! built aside and swapped in only once it validates, so a bad message leaves the last good book.

use crate::decimal::{Px, Sz};
use crate::feed::Level;
use crate::json::Value;
use crate::types::{Coin, Side};

/// The levels the feed sends per side.
pub const BOOK_LEVELS_MAX: usize = 20;

const _: () = assert!(BOOK_LEVELS_MAX <= u8::MAX as usize);
const _: () = assert!(BOOK_LEVELS_MAX > 0);

/// Why a snapshot was refused. The previous book is still in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookError {
    Malformed,
    TooManyLevels,
    /// Levels out of price order, or repeated.
    Unsorted,
    /// A level with no size or no price.
    EmptyLevel,
    /// Older than the book it would replace.
    Stale,
}

/// One book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Book {
    time_ms: u64,
    bids: [Level; BOOK_LEVELS_MAX],
    asks: [Level; BOOK_LEVELS_MAX],
    bids_count: u8,
    asks_count: u8,
}

impl Book {
    pub const EMPTY: Book = Book {
        time_ms: 0,
        bids: [Level::ZERO; BOOK_LEVELS_MAX],
        asks: [Level::ZERO; BOOK_LEVELS_MAX],
        bids_count: 0,
        asks_count: 0,
    };

    /// Replaces this book from the `data` of an `l2Book` message or result, returning its coin.
    ///
    /// A crossed book is kept, not refused: at a block boundary the feed can legitimately show one,
    /// and `is_crossed` lets the caller decide.
    pub fn apply(&mut self, data: Value<'_, '_>) -> Result<Coin, BookError> {
        let coin = data.get("coin").and_then(Value::text).and_then(Coin::parse);
        let coin = coin.ok_or(BookError::Malformed)?;
        let time_ms = data.get("time").and_then(Value::u64).ok_or(BookError::Malformed)?;
        if time_ms < self.time_ms {
            return Err(BookError::Stale);
        }

        let mut sides = data.get("levels").ok_or(BookError::Malformed)?.items();
        let bids_value = sides.next().ok_or(BookError::Malformed)?;
        let asks_value = sides.next().ok_or(BookError::Malformed)?;
        let mut next = Book { time_ms, ..Book::EMPTY };
        assert!(next.bids_count == 0);
        next.bids_count = read_side(bids_value, &mut next.bids, Side::Buy)?;
        next.asks_count = read_side(asks_value, &mut next.asks, Side::Sell)?;

        next.assert_invariants();
        assert!(next.time_ms >= self.time_ms);
        assert!(next.time_ms == time_ms);
        *self = next;
        Ok(coin)
    }

    fn assert_invariants(&self) {
        assert!(usize::from(self.bids_count) <= BOOK_LEVELS_MAX);
        assert!(usize::from(self.asks_count) <= BOOK_LEVELS_MAX);
        for pair in self.bids().windows(2) {
            assert!(pair[0].px > pair[1].px);
        }
        for pair in self.asks().windows(2) {
            assert!(pair[0].px < pair[1].px);
        }
    }

    #[must_use]
    pub const fn time_ms(&self) -> u64 {
        assert!(self.bids_count as usize <= BOOK_LEVELS_MAX);
        self.time_ms
    }

    /// Bids, best first.
    #[must_use]
    pub fn bids(&self) -> &[Level] {
        assert!(usize::from(self.bids_count) <= BOOK_LEVELS_MAX);
        &self.bids[..usize::from(self.bids_count)]
    }

    /// Asks, best first.
    #[must_use]
    pub fn asks(&self) -> &[Level] {
        assert!(usize::from(self.asks_count) <= BOOK_LEVELS_MAX);
        &self.asks[..usize::from(self.asks_count)]
    }

    #[must_use]
    pub fn best_bid(&self) -> Option<Level> {
        assert!(self.bids_count == 0 || self.bids[0].px > Px::ZERO);
        self.bids().first().copied()
    }

    #[must_use]
    pub fn best_ask(&self) -> Option<Level> {
        assert!(self.asks_count == 0 || self.asks[0].px > Px::ZERO);
        self.asks().first().copied()
    }

    #[must_use]
    pub fn is_crossed(&self) -> bool {
        assert!(self.time_ms > 0 || (self.bids_count == 0 && self.asks_count == 0));
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => bid.px >= ask.px,
            _ => false,
        }
    }

    /// The midpoint, rounded down to the raw unit. `None` until both sides have a level.
    #[must_use]
    pub fn mid(&self) -> Option<Px> {
        assert!(self.bids_count == 0 || self.bids[0].sz > Sz::ZERO);
        let bid = self.best_bid()?.px.raw();
        let ask = self.best_ask()?.px.raw();
        // Halving the gap cannot overflow the way adding the two prices could.
        let (low, high) = if bid <= ask { (bid, ask) } else { (ask, bid) };
        let mid = low + (high - low) / 2;
        assert!(mid >= low);
        assert!(mid <= high);
        Some(Px::from_raw(mid))
    }

    /// The size available through `px`: what a taker on `side` could fill at `px` or better.
    #[must_use]
    pub fn depth_through(&self, side: Side, px: Px) -> Sz {
        let levels = if side.is_buy() { self.asks() } else { self.bids() };
        assert!(levels.len() <= BOOK_LEVELS_MAX);
        let mut total = Sz::ZERO;
        for level in levels {
            assert!(level.sz > Sz::ZERO, "a stored level was validated when it was read");
            let reachable = if side.is_buy() { level.px <= px } else { level.px >= px };
            if !reachable {
                break;
            }
            total = match total.checked_add(level.sz) {
                Some(sum) => sum,
                None => return total,
            };
        }
        total
    }

    /// The worst price a taker on `side` reaches filling `sz`, or `None` if the visible book is too
    /// thin. The price to send an IOC at, once rounded for the taker with `Market::px_round`.
    #[must_use]
    pub fn impact_px(&self, side: Side, sz: Sz) -> Option<Px> {
        assert!(sz > Sz::ZERO);
        let levels = if side.is_buy() { self.asks() } else { self.bids() };
        assert!(levels.len() <= BOOK_LEVELS_MAX);
        let mut remaining = sz;
        for level in levels {
            assert!(remaining > Sz::ZERO, "a level that covered it would have returned");
            if level.sz >= remaining {
                return Some(level.px);
            }
            remaining = remaining.saturating_sub(level.sz);
        }
        None
    }
}

fn read_side(
    value: Value<'_, '_>,
    target: &mut [Level; BOOK_LEVELS_MAX],
    side: Side,
) -> Result<u8, BookError> {
    let mut count = 0usize;
    assert!(target.len() == BOOK_LEVELS_MAX);
    for item in value.items() {
        if count == BOOK_LEVELS_MAX {
            return Err(BookError::TooManyLevels);
        }
        let level = Level::parse(item).ok_or(BookError::Malformed)?;
        if level.sz == Sz::ZERO || level.px == Px::ZERO {
            return Err(BookError::EmptyLevel);
        }
        if count > 0 {
            let previous = target[count - 1].px;
            let in_order = if side.is_buy() { level.px < previous } else { level.px > previous };
            if !in_order {
                return Err(BookError::Unsorted);
            }
        }
        target[count] = level;
        count += 1;
    }
    assert!(count <= BOOK_LEVELS_MAX);
    assert!(target[..count].iter().all(|level| level.sz > Sz::ZERO));
    Ok(count as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{Token, parse};
    use core::fmt::Write;
    use std::vec;

    fn apply(book: &mut Book, text: &str) -> Result<Coin, BookError> {
        let mut tokens = vec![Token::ZERO; 512];
        let document = parse(text.as_bytes(), &mut tokens).unwrap();
        book.apply(document.root())
    }

    fn px(text: &str) -> Px {
        Px::parse(text.as_bytes()).unwrap()
    }

    fn sz(text: &str) -> Sz {
        Sz::parse(text.as_bytes()).unwrap()
    }

    const GOOD: &str = r#"{"coin":"BTC","time":10,"levels":[
        [{"px":"100","sz":"1","n":1},{"px":"99","sz":"2","n":2},{"px":"98","sz":"3","n":1}],
        [{"px":"101","sz":"1.5","n":1},{"px":"102","sz":"2.5","n":1}]]}"#;

    #[test]
    fn reads_a_snapshot_and_answers_questions_about_it() {
        let mut book = Book::EMPTY;
        assert_eq!(apply(&mut book, GOOD).unwrap().as_str(), "BTC");
        assert_eq!(book.best_bid().unwrap().px, px("100"));
        assert_eq!(book.best_ask().unwrap().px, px("101"));
        assert_eq!(book.mid(), Some(px("100.5")));
        assert!(!book.is_crossed());
        assert_eq!(book.depth_through(Side::Buy, px("101.5")), sz("1.5"));
        assert_eq!(book.depth_through(Side::Buy, px("102")), sz("4"));
        assert_eq!(book.depth_through(Side::Sell, px("99")), sz("3"));
        assert_eq!(book.depth_through(Side::Sell, px("100.5")), Sz::ZERO);
        assert_eq!(book.impact_px(Side::Buy, sz("1.5")), Some(px("101")));
        assert_eq!(book.impact_px(Side::Buy, sz("1.6")), Some(px("102")));
        assert_eq!(book.impact_px(Side::Buy, sz("4.1")), None);
        assert_eq!(book.impact_px(Side::Sell, sz("5.9")), Some(px("98")));
    }

    #[test]
    fn a_refused_snapshot_leaves_the_last_good_book() {
        let mut book = Book::EMPTY;
        apply(&mut book, GOOD).unwrap();
        let before = book;
        for (text, error) in [
            (
                r#"{"coin":"BTC","time":11,"levels":[[{"px":"99","sz":"1","n":1},{"px":"100","sz":"1","n":1}],[]]}"#,
                BookError::Unsorted,
            ),
            (
                r#"{"coin":"BTC","time":11,"levels":[[{"px":"99","sz":"1","n":1},{"px":"99","sz":"1","n":1}],[]]}"#,
                BookError::Unsorted,
            ),
            (
                r#"{"coin":"BTC","time":11,"levels":[[],[{"px":"102","sz":"1","n":1},{"px":"101","sz":"1","n":1}]]}"#,
                BookError::Unsorted,
            ),
            (
                r#"{"coin":"BTC","time":11,"levels":[[{"px":"99","sz":"0","n":1}],[]]}"#,
                BookError::EmptyLevel,
            ),
            (
                r#"{"coin":"BTC","time":11,"levels":[[{"px":"0","sz":"1","n":1}],[]]}"#,
                BookError::EmptyLevel,
            ),
            (
                r#"{"coin":"BTC","time":11,"levels":[[{"px":"x","sz":"1","n":1}],[]]}"#,
                BookError::Malformed,
            ),
            (r#"{"coin":"BTC","time":11,"levels":[[]]}"#, BookError::Malformed),
            (r#"{"coin":"BTC","levels":[[],[]]}"#, BookError::Malformed),
            (r#"{"coin":"BTC","time":9,"levels":[[],[]]}"#, BookError::Stale),
        ] {
            assert_eq!(apply(&mut book, text).unwrap_err(), error, "{text}");
            assert_eq!(book, before);
        }
    }

    #[test]
    fn levels_are_bounded() {
        let mut levels = std::string::String::new();
        for index in 0..=BOOK_LEVELS_MAX {
            if index > 0 {
                levels.push(',');
            }
            let px = 1000 - index;
            write!(levels, r#"{{"px":"{px}","sz":"1","n":1}}"#).unwrap();
        }
        let text = std::format!(r#"{{"coin":"X","time":1,"levels":[[{levels}],[]]}}"#);
        let mut book = Book::EMPTY;
        assert_eq!(apply(&mut book, &text).unwrap_err(), BookError::TooManyLevels);
    }

    #[test]
    fn empty_and_crossed_books_are_reported_and_kept() {
        let mut book = Book::EMPTY;
        assert_eq!(book.mid(), None);
        assert_eq!(book.best_bid(), None);
        assert!(!book.is_crossed());
        let crossed = r#"{"coin":"X","time":1,"levels":[[{"px":"101","sz":"1","n":1}],[{"px":"100","sz":"1","n":1}]]}"#;
        apply(&mut book, crossed).unwrap();
        assert!(book.is_crossed());
        assert_eq!(book.mid(), Some(px("100.5")));
    }

    #[test]
    fn mid_does_not_overflow_at_the_top_of_the_range() {
        let top = Level { px: Px::from_raw(u64::MAX), sz: Sz::from_raw(1), orders_count: 1 };
        let mut book = Book { bids_count: 1, asks_count: 1, ..Book::EMPTY };
        book.bids[0] = top;
        book.asks[0] = top;
        assert_eq!(book.mid(), Some(Px::from_raw(u64::MAX)));
    }
}
