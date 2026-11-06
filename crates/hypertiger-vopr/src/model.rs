//! A model of the exchange, written from the docs and captured traffic, and holding the truth the
//! oracle judges the engine against.
//!
//! It is deliberately hostile where the docs allow: it enforces the nonce window and `expiresAfter`
//! against its own clock, serves stale reads, and verifies each action's signature by re-encoding
//! the JSON it received with its own msgpack encoder. That is what the real server does, and it is
//! how a body that disagrees with the hash it was signed under gets caught.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use hypertiger::decimal::{Px, Sz, Szi};
use hypertiger::json::{Document, Entries, Items, Kind, Token, Value, parse};
use hypertiger::signing::{Signature, keccak, l1_digest, recover};
use hypertiger::types::{Address, Cloid, Network, Side};

use crate::prng::Prng;

pub const NONCES_KEPT: usize = 100;
const NONCE_BEHIND_MS: u64 = 2 * 86_400_000;
const NONCE_AHEAD_MS: u64 = 86_400_000;
/// The exchange's `userFills` limit.
const FILLS_SNAPSHOT_MAX: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    Open,
    Filled,
    Canceled,
    Rejected,
}

impl Word {
    const fn wire(self) -> &'static str {
        match self {
            Word::Open => "open",
            Word::Filled => "filled",
            Word::Canceled => "canceled",
            Word::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelOrder {
    pub cloid: Cloid,
    pub oid: u64,
    pub market: usize,
    pub side: Side,
    pub px: Px,
    pub sz_orig: u128,
    pub sz_open: u128,
    pub reduce_only: bool,
    pub landed_ms: u64,
    pub word: Word,
    /// `(time, word, sz_open)` for every change, so that a stale read can be answered honestly.
    pub history: Vec<(u64, Word, u128)>,
}

impl ModelOrder {
    fn as_of(&self, time_ms: u64) -> Option<(Word, u128)> {
        let mut found = None;
        for &(at_ms, word, open) in &self.history {
            if at_ms <= time_ms {
                found = Some((word, open));
            }
        }
        found
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FillRecord {
    pub time_ms: u64,
    pub market: usize,
    pub side: Side,
    pub px: Px,
    pub sz: u128,
    pub oid: u64,
    pub tid: u64,
    pub cloid: Cloid,
    pub start_position: i128,
}

/// What a landing did, in the odds the world drew for it.
#[derive(Clone, Copy, Debug)]
pub struct Odds {
    pub order_reject: u32,
    pub ioc_full: u32,
    pub ioc_none: u32,
    pub gtc_cross: u32,
    /// How often an IOC's remainder is reported as `filled` and not `canceled`, which mainnet's
    /// history shows.
    pub ioc_remainder_filled: u32,
}

/// The exchange's reply, and the feed messages the action caused.
#[derive(Debug)]
pub struct Effect {
    pub reply: String,
    pub feed: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Counters {
    pub actions: u64,
    pub orders_landed: u64,
    pub orders_stale: u64,
    pub orders_rejected: u64,
    pub fills: u64,
    pub cancels: u64,
    pub nonces_refused: u64,
}

/// The exchange's whole state.
#[derive(Debug)]
pub struct Model {
    pub coins: Vec<&'static str>,
    pub assets: Vec<u32>,
    pub network: Network,
    pub signer: Address,
    pub user: Address,
    pub orders: Vec<ModelOrder>,
    pub by_cloid: BTreeMap<Cloid, usize>,
    pub fills: Vec<FillRecord>,
    pub positions: Vec<i128>,
    pub nonces: BTreeSet<u64>,
    pub oid_next: u64,
    pub tid_next: u64,
    pub violations: Vec<String>,
    pub declared_never_landed: BTreeSet<Cloid>,
    pub counters: Counters,
    /// Whether the account holds a TP/SL on a whole position, set up elsewhere. On mainnet these list
    /// with `origSz` of 0.0, no client id, and `isTrigger`.
    pub foreign_position_tpsl: bool,
}

impl Model {
    #[must_use]
    pub fn new(
        coins: Vec<&'static str>,
        assets: Vec<u32>,
        network: Network,
        signer: Address,
        user: Address,
    ) -> Model {
        assert_eq!(coins.len(), assets.len());
        let market_count = coins.len();
        Model {
            coins,
            assets,
            network,
            signer,
            user,
            orders: Vec::new(),
            by_cloid: BTreeMap::new(),
            fills: Vec::new(),
            positions: vec![0; market_count],
            nonces: BTreeSet::new(),
            oid_next: 1000,
            tid_next: 1,
            violations: Vec::new(),
            declared_never_landed: BTreeSet::new(),
            counters: Counters::default(),
            foreign_position_tpsl: false,
        }
    }

    /// Lands an `/exchange` request. `None` is not possible: an action that reaches the chain is
    /// always answered, though the world may lose the answer.
    pub fn apply_action(
        &mut self,
        body: &[u8],
        now_ms: u64,
        prng: &mut Prng,
        odds: &Odds,
        tokens: &mut [Token],
    ) -> Effect {
        self.counters.actions += 1;
        let document = match parse(body, tokens) {
            Ok(document) => document,
            Err(error) => {
                self.violations.push(format!("the engine sent unparseable JSON: {error:?}"));
                return err_effect("unparseable");
            }
        };
        let root = document.root();
        if let Err(reason) = self.check_envelope(root, document, now_ms) {
            return err_effect(&reason);
        }
        let Some(action) = root.get("action") else {
            self.violations.push("an envelope without an action".to_string());
            return err_effect("no action");
        };
        match action.get("type").and_then(Value::str) {
            Some("order") => self.apply_orders(action, now_ms, prng, odds),
            Some("cancelByCloid") => self.apply_cancels(action, now_ms),
            other => {
                self.violations.push(format!("an action the engine should not send: {other:?}"));
                err_effect("unexpected action")
            }
        }
    }

    /// The checks the real exchange runs before an action does anything.
    fn check_envelope(
        &mut self,
        root: Value<'_, '_>,
        _document: Document<'_, '_>,
        now_ms: u64,
    ) -> Result<(), String> {
        let Some(nonce) = root.get("nonce").and_then(Value::u64) else {
            self.violations.push("an envelope without a nonce".to_string());
            return Err("no nonce".to_string());
        };
        let expires = root.get("expiresAfter").and_then(Value::u64);
        self.check_signature(root, nonce, expires)?;
        if let Err(reason) = self.check_nonce(nonce, now_ms) {
            self.counters.nonces_refused += 1;
            return Err(reason);
        }
        if let Some(expires) = expires {
            if now_ms > expires {
                self.counters.orders_stale += 1;
                return Err("Action expired".to_string());
            }
        }
        Ok(())
    }

    /// Re-encodes the JSON just received and recovers the signer from that. If the body and the
    /// signed hash disagree, the recovered address is wrong, exactly as on the real exchange.
    fn check_signature(
        &mut self,
        root: Value<'_, '_>,
        nonce: u64,
        expires: Option<u64>,
    ) -> Result<(), String> {
        let Some(action) = root.get("action") else {
            return Err("no action".to_string());
        };
        let mut bytes = encode_msgpack(action);
        bytes.extend_from_slice(&nonce.to_be_bytes());
        match root.get("vaultAddress").and_then(Value::text) {
            None => bytes.push(0),
            Some(text) => {
                let Some(vault) = Address::parse(text) else {
                    return Err("bad vault".to_string());
                };
                bytes.push(1);
                bytes.extend_from_slice(&vault.0);
            }
        }
        if let Some(expires) = expires {
            bytes.push(0);
            bytes.extend_from_slice(&expires.to_be_bytes());
        }
        let digest = l1_digest(&keccak(&bytes), self.network);
        let recovered = read_signature(root).and_then(|signature| recover(&digest, &signature));
        if recovered == Some(self.signer) {
            Ok(())
        } else {
            self.violations.push(format!("signature recovers to {recovered:?}"));
            Err(format!("User or API Wallet {recovered:?} does not exist."))
        }
    }

    fn check_nonce(&mut self, nonce: u64, now_ms: u64) -> Result<(), String> {
        if nonce + NONCE_BEHIND_MS <= now_ms || nonce >= now_ms + NONCE_AHEAD_MS {
            return Err("Invalid nonce: outside the window".to_string());
        }
        if self.nonces.contains(&nonce) {
            self.violations.push(format!("nonce {nonce} used twice"));
            return Err("Invalid nonce: duplicate".to_string());
        }
        if self.nonces.len() == NONCES_KEPT {
            let smallest = *self.nonces.first().expect("the set is full, so it has a first");
            if nonce <= smallest {
                return Err("Invalid nonce: too low".to_string());
            }
            self.nonces.remove(&smallest);
        }
        self.nonces.insert(nonce);
        Ok(())
    }

    fn apply_orders(
        &mut self,
        action: Value<'_, '_>,
        now_ms: u64,
        prng: &mut Prng,
        odds: &Odds,
    ) -> Effect {
        let mut statuses = Vec::new();
        let mut feed = Vec::new();
        if let Some(orders) = action.get("orders") {
            for order in orders.items() {
                let status = self.place(order, now_ms, prng, odds, &mut feed);
                statuses.push(status);
            }
        }
        let reply = format!(
            r#"{{"status":"ok","response":{{"type":"order","data":{{"statuses":[{}]}}}}}}"#,
            statuses.join(",")
        );
        Effect { reply, feed }
    }

    fn place(
        &mut self,
        wire: Value<'_, '_>,
        now_ms: u64,
        prng: &mut Prng,
        odds: &Odds,
        feed: &mut Vec<String>,
    ) -> String {
        let Some(parsed) = self.read_order(wire) else {
            self.violations.push("the engine sent an order the exchange cannot read".to_string());
            return r#"{"error":"bad order"}"#.to_string();
        };
        if self.by_cloid.contains_key(&parsed.cloid) {
            self.violations
                .push(format!("cloid {:?} landed twice: at-most-once broken", parsed.cloid));
            return r#"{"error":"duplicate cloid"}"#.to_string();
        }
        if self.declared_never_landed.contains(&parsed.cloid) {
            self.violations.push(format!("{:?} landed after being proven never to", parsed.cloid));
        }
        if prng.chance(odds.order_reject) {
            self.counters.orders_rejected += 1;
            return r#"{"error":"Insufficient margin to place order."}"#.to_string();
        }
        if parsed.reduce_only && !self.reduces(&parsed) {
            self.counters.orders_rejected += 1;
            return r#"{"error":"Reduce only order would increase position."}"#.to_string();
        }
        self.land(&parsed, now_ms, prng, odds, feed)
    }

    fn reduces(&self, order: &ParsedOrder) -> bool {
        let position = self.positions[order.market];
        if order.side.is_buy() { position < 0 } else { position > 0 }
    }

    fn read_order(&self, wire: Value<'_, '_>) -> Option<ParsedOrder> {
        let asset = u32::try_from(wire.get("a")?.u64()?).ok()?;
        let market = self.assets.iter().position(|&known| known == asset)?;
        let is_buy = wire.get("b")?.bool()?;
        let tif = wire.get("t")?.get("limit")?.get("tif")?.str()?;
        Some(ParsedOrder {
            cloid: Cloid::parse(wire.get("c")?.text()?)?,
            market,
            side: if is_buy { Side::Buy } else { Side::Sell },
            px: Px::parse(wire.get("p")?.text()?).ok()?,
            sz: Sz::parse(wire.get("s")?.text()?).ok()?.raw(),
            reduce_only: wire.get("r")?.bool()?,
            tif_ioc: tif == "Ioc",
            tif_alo: tif == "Alo",
        })
    }

    fn land(
        &mut self,
        parsed: &ParsedOrder,
        now_ms: u64,
        prng: &mut Prng,
        odds: &Odds,
        feed: &mut Vec<String>,
    ) -> String {
        self.counters.orders_landed += 1;
        let oid = self.oid_next;
        self.oid_next += 1;
        let index = self.orders.len();
        self.orders.push(ModelOrder {
            cloid: parsed.cloid,
            oid,
            market: parsed.market,
            side: parsed.side,
            px: parsed.px,
            sz_orig: parsed.sz,
            sz_open: parsed.sz,
            reduce_only: parsed.reduce_only,
            landed_ms: now_ms,
            word: Word::Open,
            history: vec![(now_ms, Word::Open, parsed.sz)],
        });
        self.by_cloid.insert(parsed.cloid, index);

        if parsed.tif_ioc {
            return self.land_ioc(index, now_ms, prng, odds, feed);
        }
        if parsed.tif_alo && prng.chance(odds.order_reject) {
            self.finish(index, Word::Rejected, now_ms, feed);
            return r#"{"error":"Post only order would have immediately matched"}"#.to_string();
        }
        feed.push(self.order_update_message(index));
        if !parsed.tif_alo && prng.chance(odds.gtc_cross) {
            let portion = self.portion(index, prng);
            self.fill(index, portion, now_ms, feed);
            if self.orders[index].sz_open == 0 {
                return filled_status(oid, parsed.sz, parsed.px, parsed.cloid);
            }
        }
        format!(r#"{{"resting":{{"oid":{oid},"cloid":"{}"}}}}"#, parsed.cloid.to_hex().as_str())
    }

    fn land_ioc(
        &mut self,
        index: usize,
        now_ms: u64,
        prng: &mut Prng,
        odds: &Odds,
        feed: &mut Vec<String>,
    ) -> String {
        let order = self.orders[index].clone();
        if prng.chance(odds.ioc_none) {
            self.finish(index, Word::Canceled, now_ms, feed);
            return r#"{"error":"Order could not immediately match against any resting orders."}"#
                .to_string();
        }
        let portion =
            if prng.chance(odds.ioc_full) { order.sz_orig } else { self.portion(index, prng) };
        self.fill(index, portion, now_ms, feed);
        if self.orders[index].word == Word::Open {
            let word =
                if prng.chance(odds.ioc_remainder_filled) { Word::Filled } else { Word::Canceled };
            self.finish(index, word, now_ms, feed);
        }
        let filled = order.sz_orig - self.orders[index].sz_open;
        // A reduce-only IOC may have been cut to nothing by the position it was reducing.
        if filled == 0 {
            return r#"{"error":"Order could not immediately match against any resting orders."}"#
                .to_string();
        }
        filled_status(order.oid, filled, order.px, order.cloid)
    }

    /// A random lot-sized portion of what remains open.
    fn portion(&self, index: usize, prng: &mut Prng) -> u128 {
        const LOT: u128 = 100_000;
        let open = self.orders[index].sz_open;
        let lots = open / LOT;
        assert!(lots > 0, "sizes are always whole lots here");
        u128::from(prng.range(1, lots as u64)) * LOT
    }

    /// Fills up to `wanted` of an order, cutting a reduce-only order at the flat position it may
    /// not pass.
    pub fn fill(&mut self, index: usize, wanted: u128, now_ms: u64, feed: &mut Vec<String>) {
        let order = self.orders[index].clone();
        assert_eq!(order.word, Word::Open);
        let start = self.positions[order.market];
        let sz = if order.reduce_only {
            let room = if order.side.is_buy() { (-start).max(0) } else { start.max(0) };
            wanted.min(order.sz_open).min(room as u128)
        } else {
            wanted.min(order.sz_open)
        };
        if sz == 0 {
            if order.reduce_only {
                self.finish(index, Word::Canceled, now_ms, feed);
            }
            return;
        }
        self.positions[order.market] =
            if order.side.is_buy() { start + sz as i128 } else { start - sz as i128 };
        let tid = self.tid_next;
        self.tid_next += 1;
        let record = FillRecord {
            time_ms: now_ms,
            market: order.market,
            side: order.side,
            px: order.px,
            sz,
            oid: order.oid,
            tid,
            cloid: order.cloid,
            start_position: start,
        };
        self.fills.push(record);
        self.counters.fills += 1;
        feed.push(self.fills_message(&[record], false));

        let open = order.sz_open - sz;
        self.orders[index].sz_open = open;
        if open == 0 {
            self.finish(index, Word::Filled, now_ms, feed);
        } else {
            self.orders[index].history.push((now_ms, Word::Open, open));
        }
    }

    fn finish(&mut self, index: usize, word: Word, now_ms: u64, feed: &mut Vec<String>) {
        let open = self.orders[index].sz_open;
        self.orders[index].word = word;
        self.orders[index].history.push((now_ms, word, open));
        feed.push(self.order_update_message(index));
    }

    fn apply_cancels(&mut self, action: Value<'_, '_>, now_ms: u64) -> Effect {
        let mut statuses = Vec::new();
        let mut feed = Vec::new();
        if let Some(cancels) = action.get("cancels") {
            for cancel in cancels.items() {
                let cloid = cancel.get("cloid").and_then(Value::text).and_then(Cloid::parse);
                statuses.push(self.cancel_one(cloid, now_ms, &mut feed));
            }
        }
        let reply = format!(
            r#"{{"status":"ok","response":{{"type":"cancel","data":{{"statuses":[{}]}}}}}}"#,
            statuses.join(",")
        );
        Effect { reply, feed }
    }

    fn cancel_one(&mut self, cloid: Option<Cloid>, now_ms: u64, feed: &mut Vec<String>) -> String {
        const MISSING: &str = r#"{"error":"Order was never placed, already canceled, or filled."}"#;
        let Some(index) = cloid.and_then(|cloid| self.by_cloid.get(&cloid).copied()) else {
            return MISSING.to_string();
        };
        if self.orders[index].word != Word::Open {
            return MISSING.to_string();
        }
        self.counters.cancels += 1;
        self.finish(index, Word::Canceled, now_ms, feed);
        r#""success""#.to_string()
    }

    /// Lets other participants trade against resting orders.
    pub fn market_step(
        &mut self,
        now_ms: u64,
        per_thousand: u32,
        prng: &mut Prng,
        feed: &mut Vec<String>,
    ) {
        for index in 0..self.orders.len() {
            if self.orders[index].word == Word::Open && prng.chance(per_thousand) {
                let portion = self.portion(index, prng);
                self.fill(index, portion, now_ms, feed);
            }
        }
    }

    fn order_update_message(&self, index: usize) -> String {
        format!(r#"{{"channel":"orderUpdates","data":[{}]}}"#, self.order_json(index, None))
    }

    fn order_json(&self, index: usize, as_of: Option<(Word, u128, u64)>) -> String {
        let order = &self.orders[index];
        let (word, open, status_ms) = as_of.unwrap_or((
            order.word,
            order.sz_open,
            order.history.last().map_or(order.landed_ms, |entry| entry.0),
        ));
        format!(
            r#"{{"order":{{"coin":"{}","side":"{}","limitPx":"{}","sz":"{}","oid":{},"timestamp":{},"origSz":"{}","cloid":"{}","reduceOnly":{}}},"status":"{}","statusTimestamp":{}}}"#,
            self.coins[order.market],
            side_letter(order.side),
            order.px,
            Sz::from_raw(open),
            order.oid,
            order.landed_ms,
            Sz::from_raw(order.sz_orig),
            order.cloid.to_hex().as_str(),
            order.reduce_only,
            word.wire(),
            status_ms
        )
    }

    fn fill_json(&self, fill: &FillRecord, text: &mut String) {
        let written = write!(
            text,
            r#"{{"coin":"{}","px":"{}","sz":"{}","side":"{}","time":{},"startPosition":"{}","dir":"","closedPnl":"0.0","hash":"0x0","oid":{},"crossed":false,"fee":"0.1","tid":{},"cloid":"{}","feeToken":"USDC"}}"#,
            self.coins[fill.market],
            fill.px,
            Sz::from_raw(fill.sz),
            side_letter(fill.side),
            fill.time_ms,
            Szi::from_raw(fill.start_position),
            fill.oid,
            fill.tid,
            fill.cloid.to_hex().as_str()
        );
        assert!(written.is_ok());
    }

    /// A `userFills` feed message.
    #[must_use]
    pub fn fills_message(&self, fills: &[FillRecord], is_snapshot: bool) -> String {
        let mut list = String::new();
        for (index, fill) in fills.iter().enumerate() {
            if index > 0 {
                list.push(',');
            }
            self.fill_json(fill, &mut list);
        }
        format!(
            r#"{{"channel":"userFills","data":{{"isSnapshot":{is_snapshot},"user":"{}","fills":[{list}]}}}}"#,
            self.user
        )
    }

    /// What a connecting feed is sent: recent fills, oldest first, flagged as a snapshot.
    #[must_use]
    pub fn feed_snapshot(&self) -> String {
        let start = self.fills.len().saturating_sub(FILLS_SNAPSHOT_MAX);
        self.fills_message(&self.fills[start..], true)
    }

    /// Answers an `/info` request as of `as_of_ms`, which is earlier than `now_ms` when this API
    /// server is behind. `clearinghouseState`'s own `time` field still says `now_ms`, as captured
    /// traffic shows, which is the unkind case for anything relying on it.
    #[must_use]
    pub fn answer_info(
        &self,
        body: &[u8],
        as_of_ms: u64,
        now_ms: u64,
        tokens: &mut [Token],
    ) -> String {
        let Ok(document) = parse(body, tokens) else {
            return r#"{"error":"unparseable"}"#.to_string();
        };
        let root = document.root();
        match root.get("type").and_then(Value::str) {
            Some("orderStatus") => self.answer_status(root, as_of_ms),
            Some("frontendOpenOrders") => self.answer_open_orders(as_of_ms),
            Some("clearinghouseState") => self.answer_positions(as_of_ms, now_ms),
            Some("userFills") => self.answer_fills(as_of_ms),
            _ => r#"{"error":"unknown"}"#.to_string(),
        }
    }

    fn answer_status(&self, root: Value<'_, '_>, as_of_ms: u64) -> String {
        let unknown = r#"{"status":"unknownOid"}"#.to_string();
        let cloid = root.get("oid").and_then(Value::text).and_then(Cloid::parse);
        let Some(&index) = cloid.and_then(|cloid| self.by_cloid.get(&cloid)) else {
            return unknown;
        };
        match self.orders[index].as_of(as_of_ms) {
            Some((word, open)) => format!(
                r#"{{"status":"order","order":{}}}"#,
                self.order_json(index, Some((word, open, as_of_ms)))
            ),
            None => unknown,
        }
    }

    fn answer_open_orders(&self, as_of_ms: u64) -> String {
        let mut list = Vec::new();
        for order in &self.orders {
            if let Some((Word::Open, open)) = order.as_of(as_of_ms) {
                list.push(format!(
                    r#"{{"coin":"{}","side":"{}","limitPx":"{}","sz":"{}","oid":{},"timestamp":{},"origSz":"{}","cloid":"{}","reduceOnly":{}}}"#,
                    self.coins[order.market],
                    side_letter(order.side),
                    order.px,
                    Sz::from_raw(open),
                    order.oid,
                    order.landed_ms,
                    Sz::from_raw(order.sz_orig),
                    order.cloid.to_hex().as_str(),
                    order.reduce_only
                ));
            }
        }
        if self.foreign_position_tpsl {
            list.push(format!(
                r#"{{"coin":"{}","side":"A","limitPx":"100","sz":"0.0","oid":999999,"timestamp":1,"origSz":"0.0","isTrigger":true,"reduceOnly":true,"cloid":null}}"#,
                self.coins[0]
            ));
        }
        format!("[{}]", list.join(","))
    }

    fn position_as_of(&self, market: usize, as_of_ms: u64) -> i128 {
        let mut position = 0i128;
        for fill in &self.fills {
            if fill.market == market && fill.time_ms <= as_of_ms {
                position = fill.start_position
                    + if fill.side.is_buy() { fill.sz as i128 } else { -(fill.sz as i128) };
            }
        }
        position
    }

    fn answer_positions(&self, as_of_ms: u64, now_ms: u64) -> String {
        let mut list = Vec::new();
        for market in 0..self.coins.len() {
            let position = self.position_as_of(market, as_of_ms);
            // A flat position is absent from the list, on the real exchange too.
            if position != 0 {
                list.push(format!(
                    r#"{{"type":"oneWay","position":{{"coin":"{}","szi":"{}"}}}}"#,
                    self.coins[market],
                    Szi::from_raw(position)
                ));
            }
        }
        format!(r#"{{"assetPositions":[{}],"time":{now_ms}}}"#, list.join(","))
    }

    fn answer_fills(&self, as_of_ms: u64) -> String {
        let mut list = String::from("[");
        let mut count = 0usize;
        for fill in self.fills.iter().rev() {
            if count == FILLS_SNAPSHOT_MAX {
                break;
            }
            if fill.time_ms <= as_of_ms {
                if count > 0 {
                    list.push(',');
                }
                self.fill_json(fill, &mut list);
                count += 1;
            }
        }
        list.push(']');
        list
    }
}

struct ParsedOrder {
    cloid: Cloid,
    market: usize,
    side: Side,
    px: Px,
    sz: u128,
    reduce_only: bool,
    tif_ioc: bool,
    tif_alo: bool,
}

fn filled_status(oid: u64, total: u128, px: Px, cloid: Cloid) -> String {
    format!(
        r#"{{"filled":{{"totalSz":"{}","avgPx":"{px}","oid":{oid},"cloid":"{}"}}}}"#,
        Sz::from_raw(total),
        cloid.to_hex().as_str()
    )
}

fn err_effect(reason: &str) -> Effect {
    Effect { reply: format!(r#"{{"status":"err","response":"{reason}"}}"#), feed: Vec::new() }
}

const fn side_letter(side: Side) -> &'static str {
    match side {
        Side::Buy => "B",
        Side::Sell => "A",
    }
}

fn read_signature(root: Value<'_, '_>) -> Option<Signature> {
    let signature = root.get("signature")?;
    Some(Signature {
        r: read_scalar(signature.get("r")?.str()?)?,
        s: read_scalar(signature.get("s")?.str()?)?,
        v: u8::try_from(signature.get("v")?.u64()?).ok()?,
    })
}

fn read_scalar(text: &str) -> Option<[u8; 32]> {
    let digits = text.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 64 {
        return None;
    }
    let padded = format!("{digits:0>64}");
    let mut scalar = [0u8; 32];
    for (index, byte) in scalar.iter_mut().enumerate() {
        *byte = u8::from_str_radix(padded.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(scalar)
}

enum Frame<'s, 't> {
    Items(Items<'s, 't>),
    Entries(Entries<'s, 't>),
}

/// Msgpack from parsed JSON, independent of `hypertiger::sink`, with an explicit stack.
#[must_use]
pub fn encode_msgpack(root: Value<'_, '_>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack: Vec<Frame<'_, '_>> = Vec::new();
    encode_one(root, &mut out, &mut stack);
    while let Some(top) = stack.last_mut() {
        match top {
            Frame::Items(items) => match items.next() {
                Some(value) => encode_one(value, &mut out, &mut stack),
                None => {
                    stack.pop();
                }
            },
            Frame::Entries(entries) => match entries.next() {
                Some((key, value)) => {
                    encode_str(key.str().expect("the engine's keys are plain text"), &mut out);
                    encode_one(value, &mut out, &mut stack);
                }
                None => {
                    stack.pop();
                }
            },
        }
    }
    out
}

fn encode_one<'s, 't>(value: Value<'s, 't>, out: &mut Vec<u8>, stack: &mut Vec<Frame<'s, 't>>) {
    match value.kind() {
        Kind::Null => out.push(0xc0),
        Kind::False => out.push(0xc2),
        Kind::True => out.push(0xc3),
        Kind::Number => encode_u64(value.u64().expect("the engine's numbers are unsigned"), out),
        Kind::String => encode_str(value.str().expect("the engine's strings are plain text"), out),
        Kind::Array => {
            let count = value.count();
            if count <= 15 {
                out.push(0x90 | count as u8);
            } else {
                out.push(0xdc);
                out.extend_from_slice(&(count as u16).to_be_bytes());
            }
            stack.push(Frame::Items(value.items()));
        }
        Kind::Object => {
            let count = value.count();
            assert!(count <= 15);
            out.push(0x80 | count as u8);
            stack.push(Frame::Entries(value.entries()));
        }
    }
}

fn encode_str(text: &str, out: &mut Vec<u8>) {
    assert!(text.len() <= 255);
    if text.len() <= 31 {
        out.push(0xa0 | text.len() as u8);
    } else {
        out.push(0xd9);
        out.push(text.len() as u8);
    }
    out.extend_from_slice(text.as_bytes());
}

fn encode_u64(value: u64, out: &mut Vec<u8>) {
    if value <= 0x7f {
        out.push(value as u8);
    } else if value <= 0xff {
        out.extend_from_slice(&[0xcc, value as u8]);
    } else if value <= 0xffff {
        out.push(0xcd);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= 0xffff_ffff {
        out.push(0xce);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&value.to_be_bytes());
    }
}
