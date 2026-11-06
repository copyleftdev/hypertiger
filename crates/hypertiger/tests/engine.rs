//! The engine's rules, one scripted world at a time.
//!
//! The simulator explores worlds at random; these state the rules outright and each one names the
//! thing that would go wrong without it. Every test plays the exchange by hand, so that a failure
//! reads as a sequence of events and not as a seed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hypertiger::action::Tif;
use hypertiger::decimal::{Px, Sz};
use hypertiger::engine::{
    CancelError, Endpoint, Engine, EngineConfig, Event, Finish, Intent, MarketConfig,
    REQUEST_SIZE_MAX, Reply, SubmitError, Transmit,
};
use hypertiger::feed::Message;
use hypertiger::json::{Token, parse};
use hypertiger::market::{Asset, GridError, Market};
use hypertiger::signing::{Signer, Wallet};
use hypertiger::types::{Address, Cloid, Coin, Network, Side};

const START_MS: u64 = 1_700_000_000_000;
const TTL_MS: u32 = 2000;
const MARGIN_MS: u32 = 1000;
const QUARANTINE_MS: u64 = 3000;

type TestEngine = Engine<8, 2>;

fn config(user: Address) -> EngineConfig {
    EngineConfig {
        network: Network::Testnet,
        user,
        vault: None,
        tick_ms: 100,
        order_ttl_ms: TTL_MS,
        settle_margin_ms: MARGIN_MS,
        request_timeout_ms: 500,
        queue_ms_max: 1500,
        status_poll_ms: 400,
        reconcile_ms: 60_000,
        fills_wait_ms: 1000,
        weight_per_minute: 800,
        weight_burst: 300,
        weight_cancel_reserve: 50,
        fast_cancel: true,
    }
}

fn sz(text: &str) -> Sz {
    Sz::parse(text.as_bytes()).unwrap()
}

fn px(text: &str) -> Px {
    Px::parse(text.as_bytes()).unwrap()
}

fn markets() -> [MarketConfig; 2] {
    let make = |coin: &str, asset: u32| MarketConfig {
        coin: Coin::parse(coin.as_bytes()).unwrap(),
        market: Market::new(Asset::from_raw(asset), 3).unwrap(),
        long_sz_max: sz("5"),
        short_sz_max: sz("5"),
    };
    [make("BTC", 0), make("ETH", 1)]
}

/// One engine and the buffers around it, with the clock in the test's hands.
struct Rig {
    engine: Box<TestEngine>,
    wallet: Wallet,
    now_ms: u64,
    target: Vec<u8>,
    tokens: Vec<Token>,
}

impl Rig {
    fn new() -> Rig {
        let wallet = Wallet::from_bytes(&[7; 32]).unwrap();
        let engine = Box::new(Engine::new(config(wallet.address()), &markets(), START_MS));
        Rig {
            engine,
            wallet,
            now_ms: START_MS,
            target: vec![0; REQUEST_SIZE_MAX],
            tokens: vec![Token::ZERO; 4096],
        }
    }

    /// A rig that has come through recovery with nothing open, ready for orders.
    fn running() -> Rig {
        let mut rig = Rig::new();
        rig.advance(QUARANTINE_MS);
        let order_read = rig.poll().expect("open orders are asked for after quarantine");
        assert!(rig.body(&order_read).contains("frontendOpenOrders"));
        rig.reply(&order_read, "[]");
        rig.advance(u64::from(MARGIN_MS));
        let positions = rig.poll().expect("positions are asked for a margin later");
        assert!(rig.body(&positions).contains("clearinghouseState"));
        let text = format!(r#"{{"assetPositions":[],"time":{}}}"#, rig.now_ms);
        rig.reply(&positions, &text);
        assert!(rig.engine.is_running());
        assert_eq!(rig.drain(), [Event::Recovered]);
        // The bucket starts empty, so give it time to earn a working budget.
        rig.advance(20_000);
        rig
    }

    fn advance(&mut self, by_ms: u64) {
        self.now_ms += by_ms;
        self.engine.tick(self.now_ms);
    }

    fn poll(&mut self) -> Option<Transmit> {
        self.engine.poll_transmit(self.now_ms, &self.wallet, &mut self.target)
    }

    fn body(&self, transmit: &Transmit) -> String {
        String::from_utf8(self.target[..transmit.size].to_vec()).unwrap()
    }

    fn reply(&mut self, transmit: &Transmit, text: &str) {
        let document = parse(text.as_bytes(), &mut self.tokens).unwrap();
        self.engine.on_reply(self.now_ms, transmit.request_id, Reply::Value(document.root()));
    }

    fn feed(&mut self, text: &str) {
        let document = parse(text.as_bytes(), &mut self.tokens).unwrap();
        let message = Message::parse(document.root()).unwrap();
        assert!(self.engine.on_message(self.now_ms, &message));
    }

    fn drain(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = self.engine.poll_event() {
            events.push(event);
        }
        events
    }

    fn submit(&mut self, intent: &Intent) -> Result<(), SubmitError> {
        self.engine.submit(intent, self.now_ms)
    }

    /// Submits an order and sends it, returning the request carrying it.
    fn place(&mut self, intent: &Intent) -> Transmit {
        self.submit(intent).unwrap();
        self.advance(100);
        let transmit = self.poll().expect("a queued order leaves on the next tick");
        assert_eq!(transmit.endpoint, Endpoint::Exchange);
        transmit
    }

    fn long_bound(&self) -> i128 {
        self.engine.exposure(0).unwrap().long_bound.raw()
    }

    fn short_bound(&self) -> i128 {
        self.engine.exposure(0).unwrap().short_bound.raw()
    }

    fn position(&self) -> i128 {
        self.engine.exposure(0).unwrap().position.raw()
    }
}

fn buy(sequence: u64, size: &str) -> Intent {
    Intent {
        cloid: Cloid::from_parts(9, sequence),
        market_index: 0,
        side: Side::Buy,
        tif: Tif::Gtc,
        px: px("100"),
        sz: sz(size),
        reduce_only: false,
    }
}

fn resting(oid: u64) -> String {
    format!(
        r#"{{"status":"ok","response":{{"type":"order","data":{{"statuses":[{{"resting":{{"oid":{oid}}}}}]}}}}}}"#
    )
}

fn fills(cloid: Cloid, side: &str, size: &str, time_ms: u64, tid: u64, start: &str) -> String {
    format!(
        r#"{{"channel":"userFills","data":{{"fills":[{{"coin":"BTC","px":"100","sz":"{size}","side":"{side}","time":{time_ms},"startPosition":"{start}","oid":77,"crossed":false,"fee":"0","tid":{tid},"cloid":"{}"}}]}}}}"#,
        cloid.to_hex().as_str()
    )
}

fn status_reply(cloid: Cloid, word: &str, remaining: &str, original: &str) -> String {
    format!(
        r#"{{"status":"order","order":{{"order":{{"coin":"BTC","side":"B","limitPx":"100","sz":"{remaining}","oid":77,"timestamp":1,"origSz":"{original}","cloid":"{}"}},"status":"{word}","statusTimestamp":1}}}}"#,
        cloid.to_hex().as_str()
    )
}

/// The same order and status as a feed message rather than a query's answer.
fn update_message(cloid: Cloid, word: &str, remaining: &str, original: &str) -> String {
    format!(
        r#"{{"channel":"orderUpdates","data":[{{"order":{{"coin":"BTC","side":"B","limitPx":"100","sz":"{remaining}","oid":77,"timestamp":1,"origSz":"{original}","cloid":"{}"}},"status":"{word}","statusTimestamp":1}}]}}"#,
        cloid.to_hex().as_str()
    )
}

const UNKNOWN: &str = r#"{"status":"unknownOid"}"#;
const SIZE_ONE: i128 = 100_000_000;

// --- Recovery -----------------------------------------------------------------------------------

/// Orders a previous life signed can land until their expiry. An engine that traded before that
/// passed could not know its own exposure.
#[test]
fn nothing_is_accepted_or_sent_during_quarantine() {
    let mut rig = Rig::new();
    assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::Recovering));
    rig.advance(QUARANTINE_MS - 1);
    assert!(rig.poll().is_none(), "no request before quarantine ends");
    assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::Recovering));
    rig.advance(1);
    assert!(rig.poll().is_some());
    assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::Recovering), "not until loaded");
}

/// Two reads see two moments. Reading positions a margin after orders is what keeps an order that
/// finished between them from being in neither.
#[test]
fn positions_are_read_a_full_margin_after_orders() {
    let mut rig = Rig::new();
    rig.advance(QUARANTINE_MS);
    let orders = rig.poll().unwrap();
    assert!(rig.poll().is_none(), "positions wait for the open orders' answer");
    rig.reply(&orders, "[]");
    rig.advance(u64::from(MARGIN_MS) - 1);
    assert!(rig.poll().is_none(), "and then for the margin");
    rig.advance(1);
    let positions = rig.poll().unwrap();
    assert!(rig.body(&positions).contains("clearinghouseState"));
    assert!(!rig.engine.is_running());
}

#[test]
fn recovery_adopts_open_orders_and_loads_positions() {
    let mut rig = Rig::new();
    rig.advance(QUARANTINE_MS);
    let cloid = Cloid::from_parts(1, 1);
    let open = format!(
        r#"[{{"coin":"BTC","side":"B","limitPx":"100","sz":"1.5","oid":55,"timestamp":1,"origSz":"2","cloid":"{}"}},
            {{"coin":"UNTRADED","side":"B","limitPx":"1","sz":"1","oid":56,"timestamp":1,"origSz":"1"}},
            {{"coin":"ETH","side":"A","limitPx":"100","sz":"1","oid":57,"timestamp":1,"origSz":"1"}}]"#,
        cloid.to_hex().as_str()
    );
    let orders = rig.poll().unwrap();
    rig.reply(&orders, &open);
    rig.advance(u64::from(MARGIN_MS));
    let positions = rig.poll().unwrap();
    let text = format!(
        r#"{{"assetPositions":[{{"position":{{"coin":"BTC","szi":"0.5"}}}}],"time":{}}}"#,
        rig.now_ms
    );
    rig.reply(&positions, &text);

    assert!(rig.engine.is_running());
    assert_eq!(rig.position(), SIZE_ONE / 2);
    assert_eq!(rig.long_bound(), SIZE_ONE * 2, "position plus the adopted order's 1.5 remaining");
    assert_eq!(rig.engine.exposure(1).unwrap().short_bound.raw(), -SIZE_ONE);
    assert_eq!(rig.engine.orders_count(), 2, "the untraded market's order is not adopted");
    let adopted = rig.engine.order_view(cloid).unwrap();
    assert_eq!(adopted.oid, 55);
    assert_eq!(adopted.sz_filled, sz("0.5"));
    assert!(adopted.is_live);
    let events = rig.drain();
    assert!(events.contains(&Event::Adopted { cloid: Some(cloid), oid: 55 }));
    assert!(events.contains(&Event::Recovered));
}

/// A failed read must be asked again, or recovery never completes.
#[test]
fn a_lost_snapshot_is_asked_for_again() {
    let mut rig = Rig::new();
    rig.advance(QUARANTINE_MS);
    let orders = rig.poll().unwrap();
    rig.engine.on_reply(rig.now_ms, orders.request_id, Reply::Failed);
    assert!(rig.poll().is_none(), "after a pause, so a broken read cannot spend the budget");
    rig.advance(400);
    let again = rig.poll().expect("open orders are wanted again");
    assert!(rig.body(&again).contains("frontendOpenOrders"));
}

// --- The ambiguity --------------------------------------------------------------------------------

/// A timeout says nothing about whether an order landed, so it must keep counting.
#[test]
fn a_timed_out_order_keeps_counting_against_the_bound() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Failed);
    assert_eq!(rig.engine.ambiguous_count(), 1);
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE, "still fully counted");
    assert_eq!(rig.drain(), [Event::Ambiguous { cloid: intent.cloid }]);
}

/// The request timeout on its own reaches the same state, with no reply at all.
#[test]
fn silence_past_the_timeout_is_ambiguity() {
    let mut rig = Rig::running();
    rig.place(&buy(1, "1"));
    rig.advance(499);
    assert_eq!(rig.engine.ambiguous_count(), 0);
    rig.advance(1);
    assert_eq!(rig.engine.ambiguous_count(), 1);
}

/// The whole design. Before expiry plus margin an unknown id can still land, so nothing may even
/// ask; once past it, the same answer proves the order never landed.
#[test]
fn unknown_means_never_landed_only_past_expiry_and_margin() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    let sent_ms = rig.now_ms;
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Failed);
    rig.drain();

    // The body carries the expiry that all of this rests on.
    let settle_ms = sent_ms + u64::from(TTL_MS) + u64::from(MARGIN_MS);
    while rig.now_ms + 100 < settle_ms {
        rig.advance(100);
        assert!(rig.poll().is_none(), "no query before the deadline, at {}", rig.now_ms - sent_ms);
    }
    rig.advance(settle_ms - rig.now_ms);
    let query = rig.poll().expect("exactly at the deadline it is worth asking");
    assert!(rig.body(&query).contains(intent.cloid.to_hex().as_str()));
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE);

    rig.reply(&query, UNKNOWN);
    assert_eq!(rig.long_bound(), 0);
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(intent.cloid),
            sz_filled: Sz::ZERO,
            finish: Finish::NeverLanded
        }]
    );
}

#[test]
fn every_order_is_sent_with_its_expiry_and_cancels_are_not() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    let body = rig.body(&sent);
    let expected = format!(r#""expiresAfter":{}"#, rig.now_ms + u64::from(TTL_MS));
    assert!(body.contains(&expected), "{body}");
    rig.reply(&sent, &resting(77));

    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    rig.advance(100);
    let cancel = rig.poll().unwrap();
    let body = rig.body(&cancel);
    assert!(body.contains(r#""expiresAfter":null"#), "{body}");
    assert!(body.contains(r#""type":"cancelByCloid""#));
    assert!(body.contains(r#""f":true"#));
}

/// Retrying an ambiguous order is how a client ends up with two of them.
#[test]
fn an_ambiguous_order_is_never_resent() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Failed);
    for _ in 0..40 {
        rig.advance(100);
        while let Some(transmit) = rig.poll() {
            let body = rig.body(&transmit);
            assert_eq!(transmit.endpoint, Endpoint::Info, "only ever a question: {body}");
            rig.reply(&transmit, &status_reply(intent.cloid, "open", "1", "1"));
        }
    }
}

/// Past the deadline the status query can also reveal that the order did land.
#[test]
fn a_status_answer_resolves_ambiguity_to_what_really_happened() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Failed);
    rig.advance(u64::from(TTL_MS + MARGIN_MS));
    let query = rig.poll().unwrap();
    rig.reply(&query, &status_reply(intent.cloid, "open", "1.5", "2"));
    let view = rig.engine.order_view(intent.cloid).unwrap();
    assert!(view.is_live);
    assert!(!view.is_ambiguous);
    assert_eq!(view.oid, 77);
}

/// A fill or an update on the feed is proof of landing, however the request looked.
#[test]
fn the_feed_resolves_ambiguity_without_being_asked() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Failed);
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 1, "0"));
    assert_eq!(rig.engine.ambiguous_count(), 0);
    assert_eq!(rig.position(), SIZE_ONE / 2);
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE, "half a unit held, one and a half still open");
}

#[test]
fn a_reply_that_cannot_be_lined_up_decides_nothing() {
    for text in [
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":1}},{"resting":{"oid":2}}]}}}"#,
        r#"{"status":"ok","response":{"type":"order"}}"#,
        r#"{"status":"weird"}"#,
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"somethingNew":{}}]}}}"#,
    ] {
        let mut rig = Rig::running();
        let sent = rig.place(&buy(1, "1"));
        rig.reply(&sent, text);
        assert_eq!(rig.engine.ambiguous_count(), 1, "{text}");
        assert_eq!(rig.long_bound(), SIZE_ONE, "{text}");
    }
}

// --- Definite answers -----------------------------------------------------------------------------

#[test]
fn a_rejection_frees_exposure_at_once() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(
        &sent,
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Insufficient margin"}]}}}"#,
    );
    assert_eq!(rig.long_bound(), 0);
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(intent.cloid),
            sz_filled: Sz::ZERO,
            finish: Finish::Rejected
        }]
    );
}

#[test]
fn a_whole_action_refused_rejects_every_order_in_it() {
    let mut rig = Rig::running();
    rig.submit(&buy(1, "1")).unwrap();
    rig.submit(&buy(2, "1")).unwrap();
    rig.advance(100);
    let sent = rig.poll().unwrap();
    assert_eq!(rig.body(&sent).matches(r#""a":0"#).count(), 2, "one batch");
    rig.reply(&sent, r#"{"status":"err","response":"User or API Wallet does not exist."}"#);
    assert_eq!(rig.engine.orders_count(), 0);
}

/// A 429 is refused at the door, so unlike a timeout it certainly did not land.
#[test]
fn a_throttle_is_a_definite_no_and_pauses_new_orders() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Throttled);
    assert_eq!(rig.engine.orders_count(), 0);
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(intent.cloid),
            sz_filled: Sz::ZERO,
            finish: Finish::Throttled
        }]
    );
    rig.submit(&buy(2, "1")).unwrap();
    rig.advance(100);
    assert!(rig.poll().is_none(), "paused for the status poll interval");
    rig.advance(300);
    assert!(rig.poll().is_some());
}

/// A partial IOC: the reply gives the total, the rest is dead, and the fill may still be on its way.
#[test]
fn an_ioc_waits_for_its_fills_while_its_final_size_counts() {
    let mut rig = Rig::running();
    let intent = Intent { tif: Tif::Ioc, ..buy(1, "2") };
    let sent = rig.place(&intent);
    rig.reply(
        &sent,
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"0.5","avgPx":"100","oid":77}}]}}}"#,
    );
    assert_eq!(rig.long_bound(), SIZE_ONE / 2, "only what filled still counts");
    assert_eq!(rig.position(), 0);
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 1, "0"));
    assert_eq!(rig.position(), SIZE_ONE / 2);
    assert_eq!(rig.long_bound(), SIZE_ONE / 2);
    assert_eq!(rig.engine.orders_count(), 0);
    let events = rig.drain();
    assert!(events.contains(&Event::Finished {
        cloid: Some(intent.cloid),
        sz_filled: sz("0.5"),
        finish: Finish::Dead,
    }));
}

/// "filled" on a resting order with less than its size means the rest is still working.
#[test]
fn a_partial_fill_on_a_resting_order_stays_live() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.reply(
        &sent,
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"0.5","avgPx":"100","oid":77}}]}}}"#,
    );
    assert!(rig.engine.order_view(intent.cloid).unwrap().is_live);
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE);
}

// --- Fills and position ---------------------------------------------------------------------------

/// A snapshot replays fills already counted, and they must not move anything twice.
#[test]
fn a_fill_counts_once_however_often_it_arrives() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    let message = fills(intent.cloid, "B", "0.5", rig.now_ms, 1, "0");
    rig.feed(&message);
    rig.feed(&message);
    rig.feed(&message);
    assert_eq!(rig.position(), SIZE_ONE / 2);
    assert_eq!(rig.engine.order_view(intent.cloid).unwrap().sz_filled, sz("0.5"));
    let count = rig.drain().iter().filter(|event| matches!(event, Event::Filled { .. })).count();
    assert_eq!(count, 1);
}

/// The exchange's `startPosition` is authoritative, so a missed fill heals on the next one.
#[test]
fn a_newer_fill_restates_the_position_past_any_gap() {
    let mut rig = Rig::running();
    let intent = buy(1, "3");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    // The first half unit of fills was never delivered, so the exchange says we start at 1.
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 2, "1"));
    assert_eq!(rig.position(), 3 * SIZE_ONE / 2);
}

/// An older fill arriving late is already inside the position a newer fill stated.
#[test]
fn an_older_fill_does_not_move_a_newer_position() {
    let mut rig = Rig::running();
    let intent = buy(1, "3");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 2, "1"));
    rig.feed(&fills(intent.cloid, "B", "1", rig.now_ms - 50, 1, "0"));
    assert_eq!(rig.position(), 3 * SIZE_ONE / 2, "1 + 0.5, and the late fill is inside that");
    assert_eq!(rig.engine.order_view(intent.cloid).unwrap().sz_filled, sz("1.5"));
}

#[test]
fn fills_in_one_block_add_up() {
    let mut rig = Rig::running();
    let intent = buy(1, "3");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 1, "0"));
    rig.feed(&fills(intent.cloid, "B", "0.25", rig.now_ms, 2, "0.5"));
    assert_eq!(rig.position(), 3 * SIZE_ONE / 4);
}

fn plain_fill(tid: u64, time_ms: u64, size: &str, start: &str) -> String {
    format!(
        r#"{{"coin":"BTC","px":"100","sz":"{size}","side":"B","time":{time_ms},"startPosition":"{start}","oid":{tid},"crossed":false,"fee":"0","tid":{tid}}}"#
    )
}

/// Runs a reconcile and answers its `userFills` request with `list`.
fn reconcile_with_fills(rig: &mut Rig, list: &str) {
    rig.engine.on_feed_reconnected(rig.now_ms);
    while let Some(transmit) = rig.poll() {
        if rig.body(&transmit).contains("userFills") {
            rig.reply(&transmit, list);
        }
    }
}

/// The feed sends fills oldest first, and they are applied as they come.
#[test]
fn a_feed_list_is_applied_oldest_first() {
    let mut rig = Rig::running();
    let first = plain_fill(1, rig.now_ms + 10, "1", "0");
    let second = plain_fill(2, rig.now_ms + 20, "2", "1");
    rig.feed(&format!(r#"{{"channel":"userFills","data":{{"fills":[{first},{second}]}}}}"#));
    assert_eq!(rig.position(), 3 * SIZE_ONE);
}

/// `/info` sends fills newest first, so its list is walked backward.
#[test]
fn an_info_list_is_applied_newest_first() {
    let mut rig = Rig::running();
    let first = plain_fill(1, rig.now_ms + 10, "1", "0");
    let second = plain_fill(2, rig.now_ms + 20, "2", "1");
    reconcile_with_fills(&mut rig, &format!("[{second},{first}]"));
    assert_eq!(rig.position(), 3 * SIZE_ONE);
}

/// Every fill in one block shares a timestamp, so nothing in the list reveals its order, and a
/// walk the wrong way counts the first fill on top of a position that already includes it. An
/// earlier version guessed from the timestamps, and a canary is what exposed it.
#[test]
fn an_info_list_from_a_single_block_is_not_counted_twice() {
    let mut rig = Rig::running();
    let same_ms = rig.now_ms + 10;
    let first = plain_fill(1, same_ms, "1", "0");
    let second = plain_fill(2, same_ms, "2", "1");
    reconcile_with_fills(&mut rig, &format!("[{second},{first}]"));
    assert_eq!(rig.position(), 3 * SIZE_ONE);
}

#[test]
fn fills_are_kept_per_market() {
    let mut rig = Rig::running();
    let eth = r#"{"channel":"userFills","data":{"fills":[{"coin":"ETH","px":"100","sz":"1","side":"B","time":TIME_NEW,"startPosition":"0","oid":1,"crossed":false,"fee":"0","tid":1}]}}"#.replace("TIME_NEW", &rig.now_ms.to_string());
    rig.feed(&eth);
    let btc = r#"{"channel":"userFills","data":{"fills":[{"coin":"BTC","px":"100","sz":"2","side":"A","time":TIME_OLD,"startPosition":"0","oid":2,"crossed":false,"fee":"0","tid":2}]}}"#.replace("TIME_OLD", &(rig.now_ms - 1000).to_string());
    rig.feed(&btc);
    assert_eq!(rig.engine.exposure(1).unwrap().position.raw(), SIZE_ONE);
    assert_eq!(rig.position(), -2 * SIZE_ONE, "an older ETH clock must not hide a BTC fill");
}

/// History from before this process anchors the position but is not news.
#[test]
fn fills_from_before_this_process_are_not_events() {
    let mut rig = Rig::running();
    let old = fills(Cloid::from_parts(1, 1), "B", "1", START_MS - 86_400_000, 1, "0");
    rig.feed(&old);
    assert_eq!(rig.position(), SIZE_ONE);
    assert_eq!(rig.drain(), []);
}

/// Its unseen size stays in the bound however long it takes.
#[test]
fn a_finished_orders_missing_fills_stay_in_the_bound() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    let update = update_message(intent.cloid, "filled", "0", "2");
    rig.feed(&update);
    for _ in 0..50 {
        rig.advance(1000);
        assert_eq!(rig.long_bound(), 2 * SIZE_ONE, "a timer must never free it");
        while let Some(transmit) = rig.poll() {
            let body = rig.body(&transmit);
            if body.contains("clearinghouseState") {
                // A read too close to the order's end proves nothing yet.
                rig.reply(&transmit, &format!(r#"{{"assetPositions":[],"time":{START_MS}}}"#));
            } else {
                rig.reply(&transmit, "[]");
            }
        }
    }
}

/// A read later than the order's end by the margin must include its fills, which ends the wait.
#[test]
fn a_new_enough_position_read_closes_a_finished_order() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    let update = update_message(intent.cloid, "filled", "0", "2");
    rig.feed(&update);
    rig.advance(1000);
    rig.advance(u64::from(MARGIN_MS));
    let mut closed = false;
    for _ in 0..8 {
        while let Some(transmit) = rig.poll() {
            let body = rig.body(&transmit);
            if body.contains("clearinghouseState") {
                let text = format!(
                    r#"{{"assetPositions":[{{"position":{{"coin":"BTC","szi":"2"}}}}],"time":{}}}"#,
                    rig.now_ms
                );
                rig.reply(&transmit, &text);
                closed = rig.engine.orders_count() == 0;
            } else {
                rig.reply(&transmit, "[]");
            }
        }
        rig.advance(1000);
    }
    assert!(closed, "the slot was returned");
    assert_eq!(rig.position(), 2 * SIZE_ONE, "and the read restated the position");
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE);
    let events = rig.drain();
    assert!(events.iter().any(|event| matches!(event, Event::FillsMissing { .. })));
    assert!(events.iter().any(|event| matches!(event, Event::PositionCorrected { .. })));
}

// --- Cancels --------------------------------------------------------------------------------------

#[test]
fn canceling_a_queued_order_leaves_no_trace_on_the_wire() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    rig.submit(&intent).unwrap();
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    assert_eq!(rig.engine.orders_count(), 0);
    rig.advance(100);
    assert!(rig.poll().is_none());
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(intent.cloid),
            sz_filled: Sz::ZERO,
            finish: Finish::CanceledInQueue
        }]
    );
}

#[test]
fn canceling_nothing_is_an_error() {
    let mut rig = Rig::running();
    assert_eq!(rig.engine.cancel(Cloid::from_parts(1, 1), rig.now_ms), Err(CancelError::NotFound));
}

/// An order can land after the cancel that was meant for it. "Never placed" is only true so far, so
/// the wish stays until the order is over.
#[test]
fn a_refused_cancel_is_tried_again_while_the_order_may_exist() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    rig.advance(100);
    let cancel = rig.poll().unwrap();
    rig.reply(
        &cancel,
        r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":[{"error":"Order was never placed, already canceled, or filled."}]}}}"#,
    );
    assert!(rig.engine.order_view(intent.cloid).unwrap().cancel_pending);
    rig.advance(400);
    let mut retried = false;
    while let Some(transmit) = rig.poll() {
        retried |= rig.body(&transmit).contains("cancelByCloid");
        if rig.body(&transmit).contains("orderStatus") {
            rig.reply(&transmit, &status_reply(intent.cloid, "open", "1", "1"));
        }
    }
    assert!(retried, "tried again");
}

#[test]
fn a_lost_cancel_is_tried_again() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    rig.advance(100);
    let cancel = rig.poll().unwrap();
    rig.engine.on_reply(rig.now_ms, cancel.request_id, Reply::Failed);
    rig.advance(400);
    let again = rig.poll().unwrap();
    assert!(rig.body(&again).contains("cancelByCloid"));
}

#[test]
fn a_successful_cancel_settles_with_what_filled() {
    let mut rig = Rig::running();
    let intent = buy(1, "2");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.feed(&fills(intent.cloid, "B", "0.5", rig.now_ms, 1, "0"));
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    rig.advance(100);
    let cancel = rig.poll().unwrap();
    rig.reply(
        &cancel,
        r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}"#,
    );
    rig.advance(400);
    let query = rig.poll().expect("asks how it ended");
    assert!(rig.body(&query).contains("orderStatus"));
    rig.reply(&query, &status_reply(intent.cloid, "canceled", "1.5", "2"));
    assert_eq!(rig.engine.orders_count(), 0);
    assert_eq!(rig.long_bound(), SIZE_ONE / 2, "only the fill remains");
}

/// Status words nobody has read must not free exposure.
#[test]
fn an_unknown_status_word_changes_nothing() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    rig.advance(100);
    let cancel = rig.poll().unwrap();
    rig.reply(
        &cancel,
        r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}"#,
    );
    rig.advance(400);
    let query = rig.poll().unwrap();
    rig.drain();
    rig.reply(&query, &status_reply(intent.cloid, "aBrandNewStatus", "0", "1"));
    assert_eq!(rig.long_bound(), SIZE_ONE);
    assert_eq!(rig.drain(), [Event::StatusUnread { cloid: intent.cloid }]);
}

// --- The gate -------------------------------------------------------------------------------------

#[test]
fn the_limit_counts_everything_open() {
    let mut rig = Rig::running();
    rig.submit(&buy(1, "3")).unwrap();
    assert_eq!(rig.submit(&buy(2, "2.001")), Err(SubmitError::RiskLimit));
    rig.submit(&buy(2, "2")).unwrap();
    assert_eq!(rig.submit(&buy(3, "0.1")), Err(SubmitError::RiskLimit));
    assert_eq!(rig.long_bound(), 5 * SIZE_ONE);
}

/// Long 3 with 5 of sells looks like -2. A reduce-only sell closing the long first leaves all 5
/// naked at -5, which is the most a further order may reach. The simulator found this case.
#[test]
fn reduce_only_orders_are_priced_as_removing_the_hedge() {
    let mut rig = Rig::running();
    let setup = fills(Cloid::from_parts(1, 1), "B", "3", rig.now_ms, 1, "0");
    rig.feed(&setup);
    assert_eq!(rig.position(), 3 * SIZE_ONE);
    let sell = |sequence: u64, size: &str, reduce_only: bool| Intent {
        side: Side::Sell,
        reduce_only,
        ..buy(sequence, size)
    };
    rig.submit(&sell(2, "5", false)).unwrap();
    assert_eq!(rig.short_bound(), -2 * SIZE_ONE);
    rig.submit(&sell(3, "3", true)).unwrap();
    assert_eq!(
        rig.submit(&sell(4, "0.1", false)),
        Err(SubmitError::RiskLimit),
        "with the long gone, 5 of sells is already at the limit"
    );
    assert_eq!(
        rig.submit(&sell(5, "0.1", true)),
        Ok(()),
        "and one that only reduces is still fine"
    );
}

#[test]
fn a_flip_within_the_limit_is_allowed() {
    let mut rig = Rig::running();
    rig.feed(&fills(Cloid::from_parts(1, 1), "B", "3", rig.now_ms, 1, "0"));
    let sell = Intent { side: Side::Sell, ..buy(2, "8") };
    assert_eq!(rig.submit(&sell), Ok(()), "from +3 a sell of 8 ends at -5");
    let more = Intent { side: Side::Sell, ..buy(3, "0.1") };
    assert_eq!(rig.submit(&more), Err(SubmitError::RiskLimit));
}

#[test]
fn intents_are_checked_before_anything_is_held() {
    let mut rig = Rig::running();
    let off_tick = Intent { px: px("100.123456"), ..buy(1, "1") };
    assert_eq!(rig.submit(&off_tick), Err(SubmitError::Grid(GridError::PxOffTick)));
    let off_lot = buy(1, "1.0001");
    assert_eq!(rig.submit(&off_lot), Err(SubmitError::Grid(GridError::SzOffLot)));
    let too_small = buy(1, "0.01");
    assert_eq!(rig.submit(&too_small), Err(SubmitError::Grid(GridError::NotionalBelowMinimum)));
    assert_eq!(
        rig.submit(&Intent { market_index: 2, ..buy(1, "1") }),
        Err(SubmitError::UnknownMarket)
    );
    rig.submit(&buy(1, "1")).unwrap();
    assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::DuplicateCloid));
    assert_eq!(rig.engine.orders_count(), 1);
}

#[test]
fn slots_are_bounded() {
    let mut rig = Rig::running();
    for sequence in 0..8 {
        rig.submit(&buy(sequence, "0.1")).unwrap();
    }
    assert_eq!(rig.submit(&buy(9, "0.1")), Err(SubmitError::SlotsFull));
}

// --- Pace ---------------------------------------------------------------------------------------

/// Nothing leaves in reaction to a call: batches go on the engine's own clock.
#[test]
fn orders_leave_on_the_tick_and_not_on_submit() {
    let mut rig = Rig::running();
    rig.advance(100);
    assert!(rig.poll().is_none(), "this tick's lanes are spent, with nothing to send");
    rig.submit(&buy(1, "1")).unwrap();
    assert!(rig.poll().is_none(), "submitted between ticks");
    rig.advance(99);
    assert!(rig.poll().is_none());
    rig.advance(1);
    assert!(rig.poll().is_some());
}

/// The docs advise batching ALO apart, because validators prioritize ALO-only batches.
#[test]
fn alo_orders_travel_in_their_own_batch() {
    let mut rig = Rig::running();
    rig.submit(&Intent { tif: Tif::Alo, ..buy(1, "1") }).unwrap();
    rig.submit(&buy(2, "1")).unwrap();
    rig.submit(&Intent { tif: Tif::Ioc, ..buy(3, "1") }).unwrap();
    rig.submit(&Intent { tif: Tif::Alo, ..buy(4, "1") }).unwrap();
    rig.advance(100);
    let first = rig.poll().unwrap();
    let body = rig.body(&first);
    assert_eq!(body.matches("Alo").count(), 2, "{body}");
    assert_eq!(body.matches("Gtc").count() + body.matches("Ioc").count(), 0);
    let second = rig.poll().unwrap();
    let body = rig.body(&second);
    assert_eq!(body.matches("Alo").count(), 0);
    assert_eq!(body.matches("Gtc").count() + body.matches("Ioc").count(), 2);
    assert!(rig.poll().is_none());
}

/// Cancels go first: they get out of the market, and the exchange prioritizes them too.
#[test]
fn cancels_leave_before_new_orders() {
    let mut rig = Rig::running();
    let first = buy(1, "1");
    let sent = rig.place(&first);
    rig.reply(&sent, &resting(77));
    rig.engine.cancel(first.cloid, rig.now_ms).unwrap();
    rig.submit(&buy(2, "1")).unwrap();
    rig.advance(100);
    let next = rig.poll().unwrap();
    assert!(rig.body(&next).contains("cancelByCloid"));
}

#[test]
fn a_stale_intent_dies_in_the_queue_rather_than_trade_late() {
    let mut rig = Rig::running();
    // Fill the budget's patience: with the throttle on, nothing can leave.
    let first = buy(1, "1");
    let sent = rig.place(&first);
    rig.engine.on_reply(rig.now_ms, sent.request_id, Reply::Throttled);
    rig.drain();
    let late = buy(2, "1");
    rig.submit(&late).unwrap();
    rig.now_ms += 1500;
    rig.engine.tick(rig.now_ms);
    assert_eq!(rig.engine.orders_count(), 0);
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(late.cloid),
            sz_filled: Sz::ZERO,
            finish: Finish::ExpiredInQueue
        }]
    );
}

#[test]
fn nonces_only_rise_even_when_the_clock_does_not() {
    let mut rig = Rig::running();
    let read_nonce = |body: &str| -> u64 {
        let at = body.find(r#""nonce":"#).unwrap() + 8;
        body[at..].split(',').next().unwrap().parse().unwrap()
    };
    let first = rig.place(&buy(1, "0.5"));
    let nonce_first = read_nonce(&rig.body(&first));
    rig.reply(&first, &resting(1));
    rig.engine.tick(rig.now_ms - 5000);
    rig.submit(&buy(2, "0.5")).unwrap();
    rig.advance(100);
    let second = rig.poll().unwrap();
    assert!(read_nonce(&rig.body(&second)) > nonce_first);
}

/// The batch is bounded, and what does not fit leaves on the next tick.
#[test]
fn a_full_batch_spills_to_the_next_tick() {
    let wallet = Wallet::from_bytes(&[7; 32]).unwrap();
    let mut large: Box<Engine<64, 1>> =
        Box::new(Engine::new(config(wallet.address()), &markets()[..1], START_MS));
    let mut tokens = vec![Token::ZERO; 256];
    let mut target = vec![0u8; REQUEST_SIZE_MAX];
    let mut now = START_MS + QUARANTINE_MS;
    large.tick(now);
    let orders = large.poll_transmit(now, &wallet, &mut target).unwrap();
    let document = parse(b"[]", &mut tokens).unwrap();
    large.on_reply(now, orders.request_id, Reply::Value(document.root()));
    now += u64::from(MARGIN_MS);
    large.tick(now);
    let positions = large.poll_transmit(now, &wallet, &mut target).unwrap();
    let text = format!(r#"{{"assetPositions":[],"time":{now}}}"#);
    let document = parse(text.as_bytes(), &mut tokens).unwrap();
    large.on_reply(now, positions.request_id, Reply::Value(document.root()));
    assert!(large.is_running());
    now += 20_000;
    large.tick(now);

    for sequence in 0..45 {
        large.submit(&buy(sequence, "0.1"), now).unwrap();
    }
    now += 100;
    large.tick(now);
    let first = large.poll_transmit(now, &wallet, &mut target).unwrap();
    let text = String::from_utf8(target[..first.size].to_vec()).unwrap();
    assert_eq!(text.matches("Gtc").count(), 39, "weight one buys 39");
    assert!(first.size <= REQUEST_SIZE_MAX);
    assert!(
        large.poll_transmit(now, &wallet, &mut target).is_none(),
        "the lane has gone this tick"
    );
    now += 100;
    large.tick(now);
    let second = large.poll_transmit(now, &wallet, &mut target).unwrap();
    let text = String::from_utf8(target[..second.size].to_vec()).unwrap();
    assert_eq!(text.matches("Gtc").count(), 6);
}

// --- The reused slot ------------------------------------------------------------------------------

/// A late reply names a slot, and a different order may have moved into it. The simulator found
/// this: without a generation, the old reply's verdict lands on the new order.
#[test]
fn a_reply_never_reaches_the_order_that_moved_into_its_slot() {
    let mut rig = Rig::running();
    let first = buy(1, "1");
    let cancel_sent = {
        let sent = rig.place(&first);
        rig.reply(&sent, &resting(77));
        rig.engine.cancel(first.cloid, rig.now_ms).unwrap();
        rig.advance(100);
        rig.poll().unwrap()
    };
    // The order ends while its cancel is still out.
    rig.feed(&update_message(first.cloid, "canceled", "1", "1"));
    assert_eq!(rig.engine.orders_count(), 0);

    let second = buy(2, "1");
    let sent = rig.place(&second);
    rig.reply(&sent, &resting(88));
    rig.drain();
    // The stale cancel's reply arrives, and says "refused".
    rig.reply(
        &cancel_sent,
        r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":[{"error":"gone"}]}}}"#,
    );
    let view = rig.engine.order_view(second.cloid).unwrap();
    assert!(!view.cancel_pending, "the new order must not inherit a cancel");
    assert!(view.is_live);
}

/// The dangerous form of the same mistake. The feed ends an order while its placement is still out,
/// and a new order moves into the slot. The old request's "rejected" must not free the new order's
/// exposure, because that order is live and the bound would then understate it.
#[test]
fn a_stale_rejection_does_not_free_the_new_orders_exposure() {
    let mut rig = Rig::running();
    let first = buy(1, "1");
    let stale = rig.place(&first);
    rig.feed(&update_message(first.cloid, "canceled", "1", "1"));
    assert_eq!(rig.engine.orders_count(), 0);

    let second = buy(2, "2");
    rig.place(&second);
    rig.drain();
    rig.reply(
        &stale,
        r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"refused"}]}}}"#,
    );
    assert_eq!(rig.engine.orders_count(), 1, "the new order is still there");
    assert_eq!(rig.long_bound(), 2 * SIZE_ONE);
    assert_eq!(rig.drain(), []);
}

// --- Conservation --------------------------------------------------------------------------------

/// When a caller stops draining, the oldest facts are kept and new risk stops.
#[test]
fn a_full_event_ring_halts_new_orders_instead_of_dropping_quietly() {
    let mut rig = Rig::running();
    for tid in 0..(hypertiger::engine::EVENTS_MAX as u64 + 5) {
        let cloid = Cloid::from_parts(1, tid);
        rig.feed(&fills(cloid, "B", "0.001", rig.now_ms + tid, tid + 1, "0"));
    }
    assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::Halted));
    assert!(rig.engine.halted().is_some());
    let events = rig.drain();
    assert_eq!(events.len(), hypertiger::engine::EVENTS_MAX);
    assert!(matches!(events[0], Event::Filled { time_ms, .. } if time_ms == rig.now_ms));
}

#[test]
fn a_feed_reconnect_compares_everything() {
    let mut rig = Rig::running();
    rig.advance(100);
    assert!(rig.poll().is_none());
    rig.engine.on_feed_reconnected(rig.now_ms);
    let mut asked = Vec::new();
    while let Some(transmit) = rig.poll() {
        asked.push(rig.body(&transmit));
    }
    assert!(asked.iter().any(|body| body.contains("frontendOpenOrders")));
    assert!(asked.iter().any(|body| body.contains("clearinghouseState")));
    assert!(asked.iter().any(|body| body.contains("userFills")));
}

/// A position read is only believed once no fill could be newer than what it saw.
#[test]
fn a_stale_position_read_cannot_overwrite_a_newer_fill() {
    let mut rig = Rig::running();
    rig.feed(&fills(Cloid::from_parts(1, 1), "B", "1", rig.now_ms, 1, "0"));
    rig.engine.on_feed_reconnected(rig.now_ms);
    let mut positions = None;
    while let Some(transmit) = rig.poll() {
        if rig.body(&transmit).contains("clearinghouseState") {
            positions = Some(transmit);
        } else {
            rig.reply(&transmit, "[]");
        }
    }
    let text = format!(r#"{{"assetPositions":[],"time":{}}}"#, rig.now_ms);
    rig.reply(&positions.unwrap(), &text);
    assert_eq!(rig.position(), SIZE_ONE, "a read within the margin of the fill proves nothing");
}

#[test]
fn a_quiet_position_read_corrects_a_drift() {
    let mut rig = Rig::running();
    let older = rig.now_ms - u64::from(MARGIN_MS) - 1;
    rig.feed(&fills(Cloid::from_parts(1, 1), "B", "1", older, 1, "0"));
    rig.drain();
    rig.engine.on_feed_reconnected(rig.now_ms);
    let mut positions = None;
    while let Some(transmit) = rig.poll() {
        if rig.body(&transmit).contains("clearinghouseState") {
            positions = Some(transmit);
        } else {
            rig.reply(&transmit, "[]");
        }
    }
    let text = format!(
        r#"{{"assetPositions":[{{"position":{{"coin":"BTC","szi":"-2"}}}}],"time":{}}}"#,
        rig.now_ms
    );
    rig.reply(&positions.unwrap(), &text);
    assert_eq!(rig.position(), -2 * SIZE_ONE);
    assert!(matches!(rig.drain()[..], [Event::PositionCorrected { market_index: 0, .. }]));
}

// --- What the exchange really sends -----------------------------------------------------------------
//
// Each of these was a crash, or a false picture of the account, until an independent review found it.
// The rule behind all of them: nothing from the network reaches an assertion, and not being able to
// read something is never the same as there being nothing there.

/// Recovers a rig, answering the open-orders read with `open`.
fn recover_with_open_orders(open: &str) -> Rig {
    let mut rig = Rig::new();
    rig.advance(QUARANTINE_MS);
    let orders = rig.poll().unwrap();
    rig.reply(&orders, open);
    rig.advance(u64::from(MARGIN_MS));
    if let Some(positions) = rig.poll() {
        let text = format!(r#"{{"assetPositions":[],"time":{}}}"#, rig.now_ms);
        rig.reply(&positions, &text);
    }
    rig
}

/// A TP/SL on a whole position, as mainnet lists it: no size of its own. Setting one in the web app
/// used to crash this engine at every start until it was removed.
#[test]
fn a_position_tpsl_with_no_size_is_harmless() {
    let open = r#"[{"coin":"BTC","side":"A","limitPx":"100","sz":"0.0","oid":77,"timestamp":1,"origSz":"0.0","isTrigger":true,"reduceOnly":true,"cloid":null}]"#;
    let mut rig = recover_with_open_orders(open);
    assert!(rig.engine.is_running());
    assert_eq!(rig.engine.orders_count(), 0);
    rig.advance(1000);
    assert_eq!(rig.long_bound(), 0);
}

#[test]
fn an_order_with_no_identity_is_not_adopted() {
    let open = r#"[{"coin":"BTC","side":"B","limitPx":"100","sz":"1","oid":0,"timestamp":1,"origSz":"1"}]"#;
    let mut rig = recover_with_open_orders(open);
    rig.advance(1000);
    assert_eq!(rig.engine.orders_count(), 0);
}

/// A sized TP/SL is counted, but the engine's fast cancel cannot reach one, so it does not pretend to.
#[test]
fn an_adopted_trigger_counts_and_is_refused_a_cancel() {
    let cloid = Cloid::from_parts(1, 1);
    let open = format!(
        r#"[{{"coin":"BTC","side":"A","limitPx":"100","sz":"1","oid":77,"timestamp":1,"origSz":"1","isTrigger":true,"cloid":"{}"}}]"#,
        cloid.to_hex().as_str()
    );
    let mut rig = recover_with_open_orders(&open);
    assert_eq!(rig.short_bound(), -SIZE_ONE);
    assert_eq!(rig.engine.cancel(cloid, rig.now_ms), Err(CancelError::Trigger));
}

/// An error where a list belongs is not an empty list. Recovery must not finish on it.
#[test]
fn an_unreadable_order_list_is_not_an_empty_one() {
    for open in [
        r#"{"error":"server busy"}"#,
        r#"[{"coin":"BTC","side":"B"}]"#,
        r#"[{"side":"B"}]"#,
        "null",
    ] {
        let mut rig = Rig::new();
        rig.advance(QUARANTINE_MS);
        let orders = rig.poll().unwrap();
        rig.reply(&orders, open);
        assert!(!rig.engine.is_running(), "{open}");
        assert_eq!(rig.engine.unreadable_count(), 1, "{open}");
        rig.advance(u64::from(MARGIN_MS));
        let again = rig.poll().expect("it is asked again");
        assert!(rig.body(&again).contains("frontendOpenOrders"), "{open}");
    }
}

/// An unreadable entry on a market this engine does not trade cannot affect the bound.
#[test]
fn an_unreadable_order_on_another_market_is_ignored() {
    let open = r#"[{"coin":"UNTRADED","side":"odd"}]"#;
    let rig = recover_with_open_orders(open);
    assert!(rig.engine.is_running());
}

/// An unreadable entry must not read as flat, which would drop a real position from the bound.
#[test]
fn an_unreadable_position_leaves_the_position_alone() {
    let mut rig = Rig::running();
    let older = rig.now_ms - u64::from(MARGIN_MS) - 1;
    rig.feed(&fills(Cloid::from_parts(1, 1), "B", "2", older, 1, "0"));
    rig.drain();
    for text in [
        format!(r#"{{"assetPositions":[{{"position":{{"coin":"BTC"}}}}],"time":{}}}"#, rig.now_ms),
        format!(r#"{{"assetPositions":"x","time":{}}}"#, rig.now_ms),
        format!(r#"{{"time":{}}}"#, rig.now_ms),
        r#"{"assetPositions":[],"time":0}"#.to_string(),
        r#"{"assetPositions":[]}"#.to_string(),
    ] {
        rig.engine.on_feed_reconnected(rig.now_ms);
        rig.advance(1000);
        while let Some(transmit) = rig.poll() {
            if rig.body(&transmit).contains("clearinghouseState") {
                rig.reply(&transmit, &text);
            } else {
                rig.reply(&transmit, "[]");
            }
        }
        assert_eq!(rig.position(), 2 * SIZE_ONE, "{text}");
    }
    assert!(rig.engine.unreadable_count() >= 5);
}

/// A timestamp from the far future would have overflowed, and one merely in the future would have
/// frozen tracking, because every real fill after it would look older.
#[test]
fn a_fill_from_the_future_is_set_aside() {
    let mut rig = Rig::running();
    for time_ms in [u64::MAX, rig.now_ms + 60_001] {
        rig.feed(&fills(Cloid::from_parts(1, 1), "B", "9", time_ms, 1, "99"));
    }
    assert_eq!(rig.position(), 0);
    assert_eq!(rig.engine.unreadable_count(), 2);
    rig.feed(&fills(Cloid::from_parts(1, 2), "B", "3", rig.now_ms, 2, "0"));
    assert_eq!(rig.position(), 3 * SIZE_ONE, "and real fills still count");
}

#[test]
fn statuses_of_the_wrong_type_decide_nothing_and_crash_nothing() {
    let mut rig = Rig::running();
    let sent = rig.place(&buy(1, "1"));
    rig.reply(&sent, r#"{"status":"ok","response":{"type":"order","data":{"statuses":{"a":1}}}}"#);
    assert_eq!(rig.engine.ambiguous_count(), 1);
    rig.advance(1000);
    assert_eq!(rig.long_bound(), SIZE_ONE);
}

/// Mainnet's history has `filled` with a remainder. Calling that all of it would report a hedge that
/// is not there.
#[test]
fn filled_with_a_remainder_is_not_all_of_it() {
    let mut rig = Rig::running();
    let intent = buy(1, "3.5");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.feed(&fills(intent.cloid, "B", "2.25", rig.now_ms, 1, "0"));
    rig.drain();
    rig.feed(&update_message(intent.cloid, "filled", "1.25", "3.5"));
    assert_eq!(rig.engine.orders_count(), 0);
    assert_eq!(
        rig.drain(),
        [Event::Finished {
            cloid: Some(intent.cloid),
            sz_filled: sz("2.25"),
            finish: Finish::Dead
        }]
    );
}

#[test]
fn a_finished_client_id_cannot_be_used_again() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.feed(&update_message(intent.cloid, "canceled", "1", "1"));
    assert_eq!(rig.engine.orders_count(), 0);
    assert_eq!(rig.submit(&intent), Err(SubmitError::DuplicateCloid));
}

/// Exposure before recovery would be zeros that mean "unknown".
#[test]
fn exposure_is_not_known_until_recovery_finishes() {
    let mut rig = Rig::new();
    assert!(rig.engine.exposure(0).is_none());
    rig.advance(QUARANTINE_MS);
    assert!(rig.engine.exposure(0).is_none());
    assert!(Rig::running().engine.exposure(0).is_some());
}

/// A cancel the exchange keeps refusing backs off instead of becoming a steady spend.
#[test]
fn a_refused_cancel_backs_off() {
    let mut rig = Rig::running();
    let intent = buy(1, "1");
    let sent = rig.place(&intent);
    rig.reply(&sent, &resting(77));
    rig.engine.cancel(intent.cloid, rig.now_ms).unwrap();
    let mut sends = 0u32;
    for _ in 0..200 {
        rig.advance(100);
        while let Some(transmit) = rig.poll() {
            if rig.body(&transmit).contains("cancelByCloid") {
                sends += 1;
                rig.reply(
                    &transmit,
                    r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":[{"error":"no"}]}}}"#,
                );
            } else {
                rig.reply(&transmit, &status_reply(intent.cloid, "open", "1", "1"));
            }
        }
    }
    assert!(sends >= 3, "it keeps trying");
    assert!(sends <= 9, "20 seconds at the base interval would be 50, and this was {sends}");
}

/// "Past the deadline" means nothing against a clock that disagrees with the exchange's.
#[test]
fn a_clock_that_disagrees_with_the_exchange_stops_new_orders() {
    for offset in [-1i64, 1] {
        let mut rig = Rig::running();
        rig.engine.on_feed_reconnected(rig.now_ms);
        while let Some(transmit) = rig.poll() {
            if rig.body(&transmit).contains("clearinghouseState") {
                let time = rig.now_ms.saturating_add_signed(offset * 5_000);
                rig.reply(&transmit, &format!(r#"{{"assetPositions":[],"time":{time}}}"#));
            } else {
                rig.reply(&transmit, "[]");
            }
        }
        assert_eq!(rig.submit(&buy(1, "1")), Err(SubmitError::Halted), "offset {offset}");
    }
}

#[test]
#[should_panic(expected = "account reads must ask about")]
fn a_vault_with_a_different_user_is_refused_at_startup() {
    let wallet = Wallet::from_bytes(&[7; 32]).unwrap();
    let mut settings = config(wallet.address());
    settings.vault = Some(Address([0x22; 20]));
    let _: TestEngine = Engine::new(settings, &markets(), START_MS);
}
