//! The world around the engine: a network with every fate, a feed that drops, a process that
//! crashes, a clock that lies. And the oracle, which checks the engine against the model's truth
//! after every step.
//!
//! The rule the network follows is the one that makes this worth running. When an outcome is
//! genuinely unknown to the engine, the world forks on the outcome and not on the report: a timed
//! out request may have died, may have landed with its reply lost, or may land later. A harness
//! that only ever picks the convenient branch tests a cooperative environment.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

use hypertiger::action::Tif;
use hypertiger::decimal::{Px, Sz};
use hypertiger::engine::{
    Endpoint, Engine, EngineConfig, Event, Finish, Intent, MarketConfig, REQUEST_SIZE_MAX, Reply,
};
use hypertiger::feed::{Message, PostKind};
use hypertiger::json::{Token, parse};
use hypertiger::market::{Asset, Market};
use hypertiger::signing::{Signer, Wallet};
use hypertiger::types::{Address, Cloid, Coin, Network, Side};

use crate::model::{Model, Odds, Word};
use crate::prng::Prng;

pub type SimEngine = Engine<64, 2>;

const STEP_MS: u64 = 10;
const COINS: [&str; 2] = ["BTC", "ETH"];
const ASSETS: [u32; 2] = [0, 1];
const LOT: u128 = 100_000;
const WIRE_ONE: u128 = 100_000_000;
const LIMIT_SZ: u128 = 5 * WIRE_ONE;
const WEIGHT_WINDOW_MS: u64 = 60_000;
const IP_WEIGHT_LIMIT: u64 = 1200;
const TOKENS_MAX: usize = 1 << 17;
/// The most clock skew the world applies: part of what `settle_margin_ms` must cover.
pub const SKEW_MS_MAX: u64 = 300;
/// How far a stale API read can lag: the rest of what it must cover.
pub const STALE_MS_MAX: u64 = 500;

const _: () = assert!(SKEW_MS_MAX + STALE_MS_MAX < 1000);

/// How one request fares. These are the worlds a timeout could have left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Arrives and is answered.
    Normal,
    /// Dies on the way. It never lands.
    LostRequest,
    /// Lands, and the reply dies. The engine cannot tell this from `LostRequest`.
    LostReply,
    /// Lands much later, maybe past the engine's patience and maybe past its own expiry.
    Delayed,
    /// Lands, and the connection reports failure at once.
    ResetAfterLanding,
    /// Never lands, and the connection reports failure at once.
    ResetBeforeLanding,
    /// Lands, and the reply arrives cut short.
    Garbled,
    /// Refused at the door with a 429.
    Throttled,
    /// Lands, and the reply arrives twice.
    DuplicateReply,
    /// Lands, and the reply is parseable JSON of a shape nobody expected.
    Malformed,
}

/// A seed's fault profile. Each seed enables its own mix (swarm testing), because the worst
/// interactions need the right faults on and the rest quiet enough to reach them.
#[derive(Clone, Copy, Debug)]
struct Profile {
    lost_request: u32,
    lost_reply: u32,
    delayed: u32,
    reset_after: u32,
    reset_before: u32,
    garbled: u32,
    throttled: u32,
    duplicate_reply: u32,
    malformed: u32,
    feed_drop: u32,
    crash: u32,
    noise: u32,
    latency_ms_max: u64,
    delay_ms_max: u64,
    skew_ms: i64,
    stale_ms_max: u64,
    market_fill: u32,
    submit_rate: u32,
    cancel_rate: u32,
    /// Sizes are drawn up to this many lots. Small sizes keep a seed trading all the way through, and
    /// large ones keep it pressed against the limit.
    size_lots_max: u64,
    odds: Odds,
}

impl Profile {
    fn draw(prng: &mut Prng) -> Profile {
        let gate = |odds_on: u32, low: u64, high: u64, prng: &mut Prng| -> u32 {
            if prng.chance(odds_on) { prng.range(low, high) as u32 } else { 0 }
        };
        Profile {
            lost_request: gate(500, 5, 80, prng),
            lost_reply: gate(500, 5, 80, prng),
            delayed: gate(500, 5, 80, prng),
            reset_after: gate(300, 5, 40, prng),
            reset_before: gate(300, 5, 40, prng),
            garbled: gate(300, 5, 40, prng),
            throttled: gate(200, 5, 30, prng),
            duplicate_reply: gate(200, 5, 40, prng),
            malformed: gate(300, 5, 40, prng),
            feed_drop: gate(500, 1, 8, prng),
            crash: gate(300, 1, 3, prng),
            noise: gate(300, 5, 50, prng),
            latency_ms_max: prng.range(5, 200),
            delay_ms_max: prng.range(500, 6000),
            skew_ms: prng.range(0, 2 * SKEW_MS_MAX) as i64 - SKEW_MS_MAX as i64,
            stale_ms_max: if prng.chance(600) { prng.range(1, STALE_MS_MAX) } else { 0 },
            market_fill: prng.range(0, 25) as u32,
            submit_rate: prng.range(20, 300) as u32,
            cancel_rate: prng.range(0, 150) as u32,
            size_lots_max: prng.range(200, 20_000),
            odds: Odds {
                order_reject: prng.range(0, 120) as u32,
                ioc_full: prng.range(100, 800) as u32,
                ioc_none: prng.range(0, 300) as u32,
                gtc_cross: prng.range(0, 300) as u32,
                ioc_remainder_filled: if prng.chance(300) {
                    prng.range(100, 1000) as u32
                } else {
                    0
                },
            },
        }
    }

    fn fate(&self, prng: &mut Prng) -> Fate {
        let draws = [
            (self.lost_request, Fate::LostRequest),
            (self.lost_reply, Fate::LostReply),
            (self.delayed, Fate::Delayed),
            (self.reset_after, Fate::ResetAfterLanding),
            (self.reset_before, Fate::ResetBeforeLanding),
            (self.garbled, Fate::Garbled),
            (self.throttled, Fate::Throttled),
            (self.duplicate_reply, Fate::DuplicateReply),
            (self.malformed, Fate::Malformed),
        ];
        for (per_thousand, fate) in draws {
            if prng.chance(per_thousand) {
                return fate;
            }
        }
        Fate::Normal
    }
}

/// How many times the rare branches actually ran. A run that never entered one proves nothing about
/// it, so the campaign requires every counter to be nonzero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub steps: u64,
    pub actions_landed: u64,
    pub orders_landed: u64,
    pub fills: u64,
    pub ambiguous: u64,
    pub never_landed: u64,
    pub landed_after_timeout: u64,
    pub expired_at_door: u64,
    pub crashes: u64,
    pub adopted: u64,
    pub feed_drops: u64,
    pub feed_snapshots: u64,
    pub throttled: u64,
    pub position_corrected: u64,
    pub fills_missing: u64,
    pub rejected: u64,
    pub risk_refused: u64,
    pub cancels_landed: u64,
    pub noise_messages: u64,
    pub reduce_only_landed: u64,
    pub malformed_replies: u64,
    pub duplicates_refused: u64,
    pub unreadable: u64,
}

impl Coverage {
    pub fn add(&mut self, other: &Coverage) {
        self.steps += other.steps;
        self.actions_landed += other.actions_landed;
        self.orders_landed += other.orders_landed;
        self.fills += other.fills;
        self.ambiguous += other.ambiguous;
        self.never_landed += other.never_landed;
        self.landed_after_timeout += other.landed_after_timeout;
        self.expired_at_door += other.expired_at_door;
        self.crashes += other.crashes;
        self.adopted += other.adopted;
        self.feed_drops += other.feed_drops;
        self.feed_snapshots += other.feed_snapshots;
        self.throttled += other.throttled;
        self.position_corrected += other.position_corrected;
        self.fills_missing += other.fills_missing;
        self.rejected += other.rejected;
        self.risk_refused += other.risk_refused;
        self.cancels_landed += other.cancels_landed;
        self.noise_messages += other.noise_messages;
        self.reduce_only_landed += other.reduce_only_landed;
        self.malformed_replies += other.malformed_replies;
        self.duplicates_refused += other.duplicates_refused;
        self.unreadable += other.unreadable;
    }

    /// The branches a campaign must have entered, by name, to mean anything. A count of zero here
    /// makes the campaign fail however clean it looks. `position_corrected` and `fills_missing` are
    /// absent on purpose: both are repairs that should almost never be needed.
    #[must_use]
    pub fn unexercised(&self) -> Vec<&'static str> {
        let checks = [
            (self.actions_landed, "actions landed"),
            (self.orders_landed, "orders landed"),
            (self.fills, "fills"),
            (self.ambiguous, "ambiguous orders"),
            (self.never_landed, "orders proven never landed"),
            (self.landed_after_timeout, "orders landing after their timeout"),
            (self.expired_at_door, "actions refused as expired"),
            (self.crashes, "crashes"),
            (self.adopted, "orders adopted after a crash"),
            (self.feed_drops, "feed drops"),
            (self.feed_snapshots, "feed snapshots"),
            (self.throttled, "throttled requests"),
            (self.rejected, "rejected orders"),
            (self.risk_refused, "risk refusals"),
            (self.cancels_landed, "cancels landed"),
            (self.noise_messages, "noise messages"),
            (self.reduce_only_landed, "reduce-only orders"),
            (self.malformed_replies, "malformed replies"),
            (self.duplicates_refused, "reused client ids refused"),
            (self.unreadable, "unreadable data set aside"),
        ];
        checks.iter().filter(|(count, _)| *count == 0).map(|(_, name)| *name).collect()
    }
}

/// What one seed came to.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub seed: u64,
    pub violation: Option<String>,
    pub trace_digest: u64,
    pub coverage: Coverage,
}

/// How long a seed runs, in simulated milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Durations {
    pub faults_ms: u64,
    pub quiet_ms: u64,
}

#[derive(Debug)]
enum EventKind {
    Arrive {
        epoch: u64,
        request_id: u64,
        endpoint: Endpoint,
        body: Vec<u8>,
        fate: Fate,
        sent_ms: u64,
    },
    ReplyText {
        epoch: u64,
        request_id: u64,
        endpoint: Endpoint,
        text: String,
    },
    ReplyFailed {
        epoch: u64,
        request_id: u64,
    },
    ReplyThrottled {
        epoch: u64,
        request_id: u64,
    },
    Feed {
        connection: u64,
        text: String,
    },
    FeedUp,
}

struct Pending {
    side: Side,
    market: usize,
    sz: u128,
    reduce_only: bool,
    expires_ms: u64,
}

/// One seed's world.
pub struct World {
    seed: u64,
    prng: Prng,
    profile: Profile,
    now_ms: u64,
    sequence: u64,
    queue: BTreeMap<(u64, u64), EventKind>,
    model: Model,
    engine: Box<SimEngine>,
    config: EngineConfig,
    markets: [MarketConfig; 2],
    wallet: Wallet,
    epoch: u64,
    connection: u64,
    feed_up: bool,
    feed_last_delivery_ms: u64,
    cloid_next: u64,
    session: u64,
    live_cloids: Vec<Cloid>,
    in_network: BTreeMap<u64, Vec<Pending>>,
    tokens: Vec<Token>,
    tokens_model: Vec<Token>,
    target: Vec<u8>,
    weights: VecDeque<(u64, u64)>,
    weight_in_window: u64,
    digest: u64,
    coverage: Coverage,
    filled_events: BTreeMap<Cloid, u128>,
    canceled_wanted: BTreeSet<Cloid>,
    settling: bool,
    trace: bool,
    /// Finished ids this life, which the strategy tries to reuse now and then.
    finished_cloids: Vec<Cloid>,
    unreadable_prior_lives: u64,
}

impl std::fmt::Debug for World {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "World(seed {}, now {})", self.seed, self.now_ms)
    }
}

/// The engine's settings in every simulation. `settle_margin_ms` must exceed the skew and the
/// staleness the world can apply; a canary lowers it and must be caught.
#[must_use]
pub fn sim_config(user: Address) -> EngineConfig {
    EngineConfig {
        network: Network::Testnet,
        user,
        vault: None,
        tick_ms: 100,
        order_ttl_ms: 2000,
        settle_margin_ms: 1000,
        request_timeout_ms: 1500,
        queue_ms_max: 3000,
        status_poll_ms: 500,
        reconcile_ms: 5000,
        fills_wait_ms: 1000,
        weight_per_minute: 800,
        weight_burst: 300,
        weight_cancel_reserve: 50,
        fast_cancel: true,
    }
}

fn sim_markets() -> [MarketConfig; 2] {
    let make = |index: usize, sz_decimals: u8| MarketConfig {
        coin: Coin::parse(COINS[index].as_bytes()).expect("short ASCII names"),
        market: Market::new(Asset::from_raw(ASSETS[index]), sz_decimals).expect("within budget"),
        long_sz_max: Sz::from_raw(LIMIT_SZ),
        short_sz_max: Sz::from_raw(LIMIT_SZ),
    };
    [make(0, 3), make(1, 3)]
}

fn fnv(digest: u64, bytes: &[u8]) -> u64 {
    let mut hash = digest;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl World {
    #[must_use]
    pub fn new(seed: u64) -> World {
        let mut prng = Prng::new(seed);
        let profile = Profile::draw(&mut prng);
        let mut secret = [0u8; 32];
        secret[24..].copy_from_slice(&(seed | 1).to_be_bytes());
        let wallet = Wallet::from_bytes(&secret).expect("a small nonzero scalar is valid");
        let now_ms = 1_700_000_000_000 + prng.range(0, 1000);
        let config = sim_config(wallet.address());
        let markets = sim_markets();
        let mut model = Model::new(
            COINS.to_vec(),
            ASSETS.to_vec(),
            config.network,
            wallet.address(),
            wallet.address(),
        );
        model.foreign_position_tpsl = prng.chance(300);
        let client_ms = client_clock(now_ms, profile.skew_ms);
        World {
            seed,
            session: prng.next_u64(),
            prng,
            profile,
            now_ms,
            sequence: 0,
            queue: BTreeMap::new(),
            model,
            engine: Box::new(Engine::new(config, &markets, client_ms)),
            config,
            markets,
            wallet,
            epoch: 0,
            connection: 0,
            feed_up: true,
            feed_last_delivery_ms: 0,
            cloid_next: 1,
            live_cloids: Vec::new(),
            in_network: BTreeMap::new(),
            tokens: vec![Token::ZERO; TOKENS_MAX],
            tokens_model: vec![Token::ZERO; TOKENS_MAX],
            target: vec![0; REQUEST_SIZE_MAX],
            weights: VecDeque::new(),
            weight_in_window: 0,
            digest: 0xcbf2_9ce4_8422_2325 ^ seed,
            coverage: Coverage::default(),
            filled_events: BTreeMap::new(),
            canceled_wanted: BTreeSet::new(),
            settling: false,
            trace: false,
            finished_cloids: Vec::new(),
            unreadable_prior_lives: 0,
        }
    }

    /// Prints every decisive event of this seed as it happens.
    #[must_use]
    pub fn traced(mut self) -> World {
        self.trace = true;
        println!("profile {:#?}", self.profile);
        self
    }

    fn say(&self, line: &str) {
        if self.trace {
            println!(
                "{:>6}.{:02} {line}",
                (self.now_ms - 1_700_000_000_000) / 1000,
                (self.now_ms % 1000) / 10
            );
        }
    }

    fn client_ms(&self) -> u64 {
        client_clock(self.now_ms, self.profile.skew_ms)
    }

    fn schedule(&mut self, at_ms: u64, kind: EventKind) {
        self.sequence += 1;
        self.queue.insert((at_ms, self.sequence), kind);
    }

    fn latency(&mut self) -> u64 {
        self.prng.range(1, self.profile.latency_ms_max)
    }

    /// Runs the whole seed.
    #[must_use]
    pub fn run(mut self, durations: Durations) -> Outcome {
        let start_ms = self.now_ms;
        let faults_end_ms = start_ms + durations.faults_ms;
        let end_ms = faults_end_ms + durations.quiet_ms;
        let mut violation = None;
        while self.now_ms < end_ms {
            if self.now_ms >= faults_end_ms && !self.settling {
                self.begin_settling();
            }
            if let Err(found) = self.step() {
                violation = Some(found);
                break;
            }
        }
        if violation.is_none() {
            violation = self.check_converged().err();
        }
        if violation.is_none() {
            violation = self.model.violations.first().cloned();
        }
        self.collect_model_coverage();
        Outcome { seed: self.seed, violation, trace_digest: self.digest, coverage: self.coverage }
    }

    fn collect_model_coverage(&mut self) {
        let counters = &self.model.counters;
        self.coverage.actions_landed = counters.actions;
        self.coverage.orders_landed = counters.orders_landed;
        self.coverage.fills = counters.fills;
        self.coverage.expired_at_door = counters.orders_stale;
        self.coverage.cancels_landed = counters.cancels;
        self.coverage.unreadable = self.unreadable_prior_lives + self.engine.unreadable_count();
        self.coverage.reduce_only_landed =
            self.model.orders.iter().filter(|order| order.reduce_only).count() as u64;
    }

    /// Stops the faults and has the strategy withdraw, so that everything must converge.
    fn begin_settling(&mut self) {
        self.say("faults stop");
        self.settling = true;
        self.profile.lost_request = 0;
        self.profile.lost_reply = 0;
        self.profile.delayed = 0;
        self.profile.reset_after = 0;
        self.profile.reset_before = 0;
        self.profile.garbled = 0;
        self.profile.throttled = 0;
        self.profile.duplicate_reply = 0;
        self.profile.malformed = 0;
        self.profile.feed_drop = 0;
        self.profile.crash = 0;
        self.profile.noise = 0;
        if !self.feed_up {
            self.feed_reconnect();
        }
    }

    fn step(&mut self) -> Result<(), String> {
        self.now_ms += STEP_MS;
        self.coverage.steps += 1;
        self.deliver_due();
        self.step_market();
        self.step_faults();
        self.step_strategy();
        self.step_engine()?;
        self.check_truth()
    }

    fn deliver_due(&mut self) {
        // Bounded by the queue, which only this loop's handlers add to, and only for the future.
        while let Some(entry) = self.queue.first_entry() {
            if entry.key().0 > self.now_ms {
                break;
            }
            let kind = entry.remove();
            self.deliver(kind);
        }
    }

    fn deliver(&mut self, kind: EventKind) {
        match kind {
            EventKind::Arrive { epoch, request_id, endpoint, body, fate, sent_ms } => {
                self.arrive(epoch, request_id, endpoint, &body, fate, sent_ms);
            }
            EventKind::ReplyText { epoch, request_id, endpoint, text } => {
                if epoch == self.epoch {
                    self.reply_text(request_id, endpoint, &text);
                }
            }
            EventKind::ReplyFailed { epoch, request_id } => {
                if epoch == self.epoch {
                    let now = self.client_ms();
                    self.engine.on_reply(now, request_id, Reply::Failed);
                }
            }
            EventKind::ReplyThrottled { epoch, request_id } => {
                if epoch == self.epoch {
                    let now = self.client_ms();
                    self.engine.on_reply(now, request_id, Reply::Throttled);
                }
            }
            EventKind::Feed { connection, text } => {
                if connection == self.connection && self.feed_up {
                    self.feed_deliver(&text);
                }
            }
            EventKind::FeedUp => self.feed_reconnect(),
        }
    }

    fn reply_text(&mut self, request_id: u64, endpoint: Endpoint, text: &str) {
        self.digest = fnv(self.digest, text.as_bytes());
        self.say(&format!("reply {request_id} {}", &text[..text.len().min(300)]));
        let now = self.client_ms();
        match parse(text.as_bytes(), &mut self.tokens) {
            Ok(document) => {
                // A post's `type` distinguishes these on the real socket. Over HTTP the endpoint
                // does, and these replies carry no wrapper.
                let _ = (endpoint, PostKind::Info);
                self.engine.on_reply(now, request_id, Reply::Value(document.root()));
            }
            Err(_) => self.engine.on_reply(now, request_id, Reply::Failed),
        }
    }

    fn feed_deliver(&mut self, text: &str) {
        self.digest = fnv(self.digest, text.as_bytes());
        self.say(&format!("feed {}", &text[..text.len().min(360)]));
        let now = self.client_ms();
        let Ok(document) = parse(text.as_bytes(), &mut self.tokens) else {
            return;
        };
        if let Some(message) = Message::parse(document.root()) {
            let _ = self.engine.on_message(now, &message);
        }
    }

    /// Delivers in order on a connection, as TCP would.
    fn feed_send(&mut self, text: String) {
        if self.feed_up {
            let latency = self.latency();
            let at_ms = (self.now_ms + latency).max(self.feed_last_delivery_ms);
            self.feed_last_delivery_ms = at_ms;
            let connection = self.connection;
            self.schedule(at_ms, EventKind::Feed { connection, text });
        }
    }

    fn feed_reconnect(&mut self) {
        self.feed_up = true;
        self.connection += 1;
        self.feed_last_delivery_ms = self.now_ms;
        self.coverage.feed_snapshots += 1;
        let now = self.client_ms();
        self.engine.on_feed_reconnected(now);
        let snapshot = self.model.feed_snapshot();
        self.feed_send(snapshot);
    }

    fn arrive(
        &mut self,
        epoch: u64,
        request_id: u64,
        endpoint: Endpoint,
        body: &[u8],
        fate: Fate,
        sent_ms: u64,
    ) {
        let pending_key = sent_ms ^ (request_id << 40) ^ (epoch << 56);
        self.in_network.remove(&pending_key);
        let text = match endpoint {
            Endpoint::Exchange => self.arrive_action(body, fate),
            Endpoint::Info => {
                let lag = if self.profile.stale_ms_max > 0 {
                    self.prng.range(0, self.profile.stale_ms_max)
                } else {
                    0
                };
                let as_of = self.now_ms.saturating_sub(lag);
                let text = self.model.answer_info(body, as_of, self.now_ms, &mut self.tokens_model);
                self.charge_surcharge(&text, body);
                text
            }
        };
        self.reply_with_fate(epoch, request_id, endpoint, text, fate);
    }

    fn arrive_action(&mut self, body: &[u8], fate: Fate) -> String {
        let odds = self.profile.odds;
        let effect = self.model.apply_action(
            body,
            self.now_ms,
            &mut self.prng,
            &odds,
            &mut self.tokens_model,
        );
        if fate == Fate::Delayed {
            self.coverage.landed_after_timeout += 1;
        }
        for text in effect.feed {
            self.feed_send(text);
        }
        effect.reply
    }

    fn reply_with_fate(
        &mut self,
        epoch: u64,
        request_id: u64,
        endpoint: Endpoint,
        text: String,
        fate: Fate,
    ) {
        let latency = self.latency();
        let at_ms = self.now_ms + latency;
        match fate {
            Fate::LostReply | Fate::LostRequest | Fate::ResetBeforeLanding => {}
            Fate::ResetAfterLanding | Fate::Throttled => {
                self.schedule(at_ms, EventKind::ReplyFailed { epoch, request_id });
            }
            Fate::Garbled => {
                let cut = text.len() / 2;
                let text = text[..cut].to_string();
                self.schedule(at_ms, EventKind::ReplyText { epoch, request_id, endpoint, text });
            }
            Fate::Malformed => {
                self.coverage.malformed_replies += 1;
                let text = self.malformed_reply();
                self.schedule(at_ms, EventKind::ReplyText { epoch, request_id, endpoint, text });
            }
            Fate::DuplicateReply => {
                let copy = text.clone();
                self.schedule(
                    at_ms,
                    EventKind::ReplyText { epoch, request_id, endpoint, text: copy },
                );
                self.schedule(
                    at_ms + latency,
                    EventKind::ReplyText { epoch, request_id, endpoint, text },
                );
            }
            Fate::Normal | Fate::Delayed => {
                self.schedule(at_ms, EventKind::ReplyText { epoch, request_id, endpoint, text });
            }
        }
    }

    /// Parseable, and wrong. These are shapes and not lies: a well-formed reply saying something false
    /// cannot be defended against, and TLS is what rules that out. What must never happen is one of
    /// these being read as "nothing there", or reaching an assertion.
    fn malformed_reply(&mut self) -> String {
        const SHAPES: [&str; 12] = [
            r#"{"error":"server busy"}"#,
            r#""oops""#,
            "null",
            "17",
            r#"{"status":"weird"}"#,
            r#"{"assetPositions":"x","time":5}"#,
            r#"{"assetPositions":[],"time":0}"#,
            r#"{"assetPositions":[{"position":{"coin":"BTC"}}],"time":5}"#,
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":{"a":1}}}}"#,
            r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":{"a":1}}}}"#,
            r#"[{"coin":"BTC","side":"B"}]"#,
            r#"[{"side":"B"}]"#,
        ];
        SHAPES[self.prng.index(SHAPES.len())].to_string()
    }

    fn step_market(&mut self) {
        let mut feed = Vec::new();
        let rate = self.profile.market_fill;
        self.model.market_step(self.now_ms, rate, &mut self.prng, &mut feed);
        for text in feed {
            self.feed_send(text);
        }
    }

    fn step_faults(&mut self) {
        if self.feed_up && self.prng.chance(self.profile.feed_drop) && self.prng.chance(100) {
            self.feed_up = false;
            self.coverage.feed_drops += 1;
            let delay = self.prng.range(100, 3000);
            self.schedule(self.now_ms + delay, EventKind::FeedUp);
        }
        if self.prng.chance(self.profile.crash) && self.prng.chance(20) {
            self.crash();
        }
        if self.prng.chance(self.profile.noise) {
            self.send_noise();
        }
    }

    /// The process dies. Requests already sent keep travelling and may still land: the worst thing
    /// a restart can forget.
    fn crash(&mut self) {
        self.say("CRASH");
        self.coverage.crashes += 1;
        self.epoch += 1;
        let now = self.client_ms();
        self.unreadable_prior_lives += self.engine.unreadable_count();
        *self.engine = Engine::new(self.config, &self.markets, now);
        self.live_cloids.clear();
        self.canceled_wanted.clear();
        self.filled_events.clear();
        self.finished_cloids.clear();
        // The exchange's budget is per IP, so it does not reset because a process did.
        self.feed_reconnect();
    }

    fn send_noise(&mut self) {
        const NOISE: [&str; 11] = [
            r#"{"channel":"somethingNew","data":{"x":1}}"#,
            r#"{"channel":"orderUpdates","data":[{"order":{"coin":"BTC"},"status":"open"}]}"#,
            r#"{"channel":"userFills","data":{"fills":[{"coin":"BTC","px":"x"}]}}"#,
            "not json at all",
            r#"{"channel":"orderUpdates","data":"oops"}"#,
            r#"{"data":1}"#,
            r#"{"channel":"userFills","data":{"fills":{"a":1}}}"#,
            r#"{"channel":"orderUpdates","data":[{"order":{"coin":"BTC","side":"B","limitPx":"1","sz":"1","oid":0,"timestamp":1,"origSz":"1"},"status":"open","statusTimestamp":1}]}"#,
            r#"{"channel":"userFills","data":{"fills":[{"coin":"BTC","px":"1","sz":"1","side":"B","time":18446744073709551615,"startPosition":"999","oid":1,"tid":1}]}}"#,
            r#"{"channel":"userFills","data":{"fills":[{"coin":"NOTOURS","px":"1","sz":"1","side":"B","time":5,"oid":1,"tid":1}]}}"#,
            r#"{"channel":"orderUpdates","data":[{"order":{"coin":"BTC","side":"B","limitPx":"1","sz":"0","oid":18446744073709551615,"timestamp":1,"origSz":"0"},"status":"aBrandNewWord","statusTimestamp":0}]}"#,
        ];
        self.coverage.noise_messages += 1;
        let text = NOISE[self.prng.index(NOISE.len())].to_string();
        self.feed_send(text);
    }

    fn step_strategy(&mut self) {
        if self.settling {
            self.withdraw();
            return;
        }
        if self.prng.chance(self.profile.submit_rate) {
            self.submit_random();
        }
        if !self.finished_cloids.is_empty() && self.prng.chance(5) {
            self.submit_reused();
        }
        if self.prng.chance(self.profile.cancel_rate) && !self.live_cloids.is_empty() {
            let index = self.prng.index(self.live_cloids.len());
            let cloid = self.live_cloids[index];
            let now = self.client_ms();
            if self.engine.cancel(cloid, now).is_ok() {
                self.canceled_wanted.insert(cloid);
            }
        }
    }

    fn withdraw(&mut self) {
        for index in 0..self.live_cloids.len() {
            let cloid = self.live_cloids[index];
            let now = self.client_ms();
            let _ = self.engine.cancel(cloid, now);
        }
    }

    /// A careless caller. Sent, this would be the same id on the wire twice.
    fn submit_reused(&mut self) {
        let cloid = self.finished_cloids[self.prng.index(self.finished_cloids.len())];
        let intent = Intent {
            cloid,
            market_index: 0,
            side: Side::Buy,
            tif: Tif::Gtc,
            px: Px::from_raw(100 * hypertiger::decimal::WIRE_ONE),
            sz: Sz::from_raw(100 * LOT * 10),
            reduce_only: false,
        };
        let now = self.client_ms();
        if self.engine.submit(&intent, now) == Err(hypertiger::engine::SubmitError::DuplicateCloid)
        {
            self.coverage.duplicates_refused += 1;
        }
    }

    fn submit_random(&mut self) {
        let market_index = self.prng.range(0, 1) as u16;
        let side = if self.prng.chance(500) { Side::Buy } else { Side::Sell };
        let tif = match self.prng.range(0, 2) {
            0 => Tif::Alo,
            1 => Tif::Ioc,
            _ => Tif::Gtc,
        };
        let lots = self.prng.range(100, self.profile.size_lots_max);
        let cloid = Cloid::from_parts(self.session, self.cloid_next);
        self.cloid_next += 1;
        let intent = Intent {
            cloid,
            market_index,
            side,
            tif,
            px: Px::from_raw(self.prng.range(100, 200) * hypertiger::decimal::WIRE_ONE),
            sz: Sz::from_raw(u128::from(lots) * LOT),
            reduce_only: self.prng.chance(150),
        };
        let now = self.client_ms();
        match self.engine.submit(&intent, now) {
            Ok(()) => self.live_cloids.push(cloid),
            Err(hypertiger::engine::SubmitError::RiskLimit) => self.coverage.risk_refused += 1,
            Err(_) => {}
        }
    }

    fn step_engine(&mut self) -> Result<(), String> {
        let now = self.client_ms();
        self.engine.tick(now);
        // Bounded: the engine refuses once every request slot is full, and at most a few batches
        // are due per tick.
        for _ in 0..hypertiger::engine::REQUESTS_MAX {
            let Some(transmit) = self.engine.poll_transmit(now, &self.wallet, &mut self.target)
            else {
                break;
            };
            let body = self.target[..transmit.size].to_vec();
            self.digest = fnv(self.digest, &body);
            self.send(transmit.request_id, transmit.endpoint, body)?;
        }
        self.drain_events();
        Ok(())
    }

    fn send(&mut self, request_id: u64, endpoint: Endpoint, body: Vec<u8>) -> Result<(), String> {
        self.charge_weight(&body, endpoint)?;
        let fate = self.profile.fate(&mut self.prng);
        self.say(&format!(
            "send {request_id} {fate:?} {}",
            String::from_utf8_lossy(&body[..body.len().min(420)])
        ));
        let epoch = self.epoch;
        match fate {
            Fate::LostRequest => {}
            Fate::Throttled => {
                self.coverage.throttled += 1;
                let at_ms = self.now_ms + self.latency();
                self.schedule(at_ms, EventKind::ReplyThrottled { epoch, request_id });
            }
            Fate::ResetBeforeLanding => {
                let at_ms = self.now_ms + self.latency();
                self.schedule(at_ms, EventKind::ReplyFailed { epoch, request_id });
            }
            _ => {
                let delay = if fate == Fate::Delayed {
                    self.prng.range(500, self.profile.delay_ms_max)
                } else {
                    self.latency()
                };
                self.track_in_network(epoch, request_id, endpoint, &body);
                let sent_ms = self.now_ms;
                self.schedule(
                    self.now_ms + delay,
                    EventKind::Arrive { epoch, request_id, endpoint, body, fate, sent_ms },
                );
            }
        }
        Ok(())
    }

    /// Remembers orders on the wire, which the oracle counts as exposure until they land or expire.
    fn track_in_network(&mut self, epoch: u64, request_id: u64, endpoint: Endpoint, body: &[u8]) {
        if endpoint != Endpoint::Exchange {
            return;
        }
        let Ok(document) = parse(body, &mut self.tokens_model) else {
            return;
        };
        let root = document.root();
        let Some(orders) = root.get("action").and_then(|action| action.get("orders")) else {
            return;
        };
        let expires = root.get("expiresAfter").and_then(hypertiger::json::Value::u64).unwrap_or(0);
        let mut list = Vec::new();
        for order in orders.items() {
            let market =
                order.get("a").and_then(hypertiger::json::Value::u64).unwrap_or(0) as usize;
            let is_buy = order.get("b").and_then(hypertiger::json::Value::bool).unwrap_or(true);
            let size = order
                .get("s")
                .and_then(hypertiger::json::Value::text)
                .and_then(|text| Sz::parse(text).ok())
                .map_or(0, Sz::raw);
            let reduce_only =
                order.get("r").and_then(hypertiger::json::Value::bool).unwrap_or(false);
            list.push(Pending {
                side: if is_buy { Side::Buy } else { Side::Sell },
                market,
                sz: size,
                reduce_only,
                expires_ms: expires,
            });
        }
        let key = self.now_ms ^ (request_id << 40) ^ (epoch << 56);
        self.in_network.insert(key, list);
    }

    /// The docs add weight per 20 items on a fill list, known only once the list exists.
    fn charge_surcharge(&mut self, text: &str, body: &[u8]) {
        if body.windows(9).any(|window| window == b"userFills") {
            let items = text.matches("\"tid\":").count() as u64;
            self.weights.push_back((self.now_ms, items / 20));
            self.weight_in_window += items / 20;
        }
    }

    /// The exchange's REST budget, over a true sliding window: the limit the bucket must respect.
    fn charge_weight(&mut self, body: &[u8], endpoint: Endpoint) -> Result<(), String> {
        let weight = request_weight(body, endpoint, &mut self.tokens_model);
        self.weights.push_back((self.now_ms, weight));
        self.weight_in_window += weight;
        while let Some(&(at_ms, old)) = self.weights.front() {
            if at_ms + WEIGHT_WINDOW_MS <= self.now_ms {
                self.weights.pop_front();
                self.weight_in_window -= old;
            } else {
                break;
            }
        }
        if self.weight_in_window > IP_WEIGHT_LIMIT {
            return Err(format!(
                "sent {} weight in a sliding minute, past the exchange's {IP_WEIGHT_LIMIT}",
                self.weight_in_window
            ));
        }
        Ok(())
    }

    fn drain_events(&mut self) {
        // Bounded by the engine's event ring.
        for _ in 0..hypertiger::engine::EVENTS_MAX {
            let Some(event) = self.engine.poll_event() else {
                break;
            };
            self.note_event(&event);
        }
    }

    fn note_event(&mut self, event: &Event) {
        let mut text = String::new();
        let written = write!(text, "{event:?}");
        assert!(written.is_ok());
        self.say(&format!("event {text}"));
        self.digest = fnv(self.digest, text.as_bytes());
        match *event {
            Event::Ambiguous { .. } => self.coverage.ambiguous += 1,
            Event::Adopted { .. } => self.coverage.adopted += 1,
            Event::PositionCorrected { .. } => self.coverage.position_corrected += 1,
            Event::FillsMissing { .. } => self.coverage.fills_missing += 1,
            Event::Filled { cloid: Some(cloid), sz, .. } => {
                *self.filled_events.entry(cloid).or_insert(0) += sz.raw();
            }
            Event::Finished { cloid: Some(cloid), finish, .. } => {
                self.note_finish(cloid, finish);
            }
            _ => {}
        }
    }

    fn note_finish(&mut self, cloid: Cloid, finish: Finish) {
        self.live_cloids.retain(|known| *known != cloid);
        if self.finished_cloids.len() < 64 {
            self.finished_cloids.push(cloid);
        }
        match finish {
            Finish::NeverLanded => {
                self.coverage.never_landed += 1;
                if self.model.by_cloid.contains_key(&cloid) {
                    self.model
                        .violations
                        .push(format!("{cloid:?} proven never landed, and it did"));
                }
                self.model.declared_never_landed.insert(cloid);
            }
            Finish::Rejected => self.coverage.rejected += 1,
            Finish::Filled
            | Finish::Dead
            | Finish::Throttled
            | Finish::CanceledInQueue
            | Finish::ExpiredInQueue => {}
        }
    }

    /// The heart of it: the engine's bounds against the exchange's truth, at every step.
    fn check_truth(&self) -> Result<(), String> {
        for market in 0..COINS.len() {
            let position = self.model.positions[market];
            let mut possible = Possible {
                long_all: position,
                short_all: position,
                long_gated: position,
                short_gated: position,
            };
            for order in self.model.orders.iter().filter(|order| order.market == market) {
                if order.word == Word::Open {
                    possible.add(order.side, order.sz_open as i128, order.reduce_only);
                }
            }
            for pending in self.in_network.values().flatten() {
                let may_land = pending.expires_ms + SKEW_MS_MAX >= self.now_ms;
                if pending.market == market && may_land {
                    possible.add(pending.side, pending.sz as i128, pending.reduce_only);
                }
            }
            self.check_limits(market, possible.long_gated, possible.short_gated)?;
            if self.engine.is_running() {
                self.check_bounds(market, possible.long_all, possible.short_all)?;
            }
        }
        match self.model.violations.first() {
            Some(found) => Err(found.clone()),
            None => Ok(()),
        }
    }

    fn check_limits(&self, market: usize, long: i128, short: i128) -> Result<(), String> {
        // Only orders that can grow a position are gated. A crash can leave the old life's orders
        // on the wire against an unchanged limit, and quarantine is what stops new ones joining.
        if long > LIMIT_SZ as i128 {
            return Err(format!(
                "{}: truly long {long}, past the limit\n{}",
                COINS[market],
                self.explain(market)
            ));
        }
        if short < -(LIMIT_SZ as i128) {
            return Err(format!(
                "{}: truly short {short}, past the limit\n{}",
                COINS[market],
                self.explain(market)
            ));
        }
        Ok(())
    }

    fn explain(&self, market: usize) -> String {
        let mut text = format!(
            "position {} engine {:?}\n",
            self.model.positions[market],
            self.engine.exposure(market as u16)
        );
        for order in self.model.orders.iter().filter(|o| o.market == market && o.word == Word::Open)
        {
            let _ = writeln!(
                text,
                "open {:?} {:?} open {} ro {} view {:?}",
                order.cloid,
                order.side,
                order.sz_open,
                order.reduce_only,
                self.engine.order_view(order.cloid)
            );
        }
        for (key, list) in &self.in_network {
            for pending in list.iter().filter(|p| p.market == market) {
                let _ = writeln!(
                    text,
                    "wire {key} {:?} {} ro {} expires in {}",
                    pending.side,
                    pending.sz,
                    pending.reduce_only,
                    i128::from(pending.expires_ms) - i128::from(self.now_ms)
                );
            }
        }
        text
    }

    fn check_bounds(&self, market: usize, long: i128, short: i128) -> Result<(), String> {
        let Some(exposure) = self.engine.exposure(market as u16) else {
            return Err("a running engine must know its exposure".to_string());
        };
        if exposure.long_bound.raw() < long {
            return Err(format!(
                "{}: the engine's long bound {} is below the possible {long}\n{}",
                COINS[market],
                exposure.long_bound.raw(),
                self.explain(market)
            ));
        }
        if exposure.short_bound.raw() > short {
            return Err(format!(
                "{}: the engine's short bound {} is above the possible {short}\n{}",
                COINS[market],
                exposure.short_bound.raw(),
                self.explain(market)
            ));
        }
        Ok(())
    }

    /// Once the faults stop, everything must come out even: no open questions, and the engine's
    /// picture equal to the exchange's.
    fn check_converged(&self) -> Result<(), String> {
        if !self.engine.is_settled() {
            return Err(format!(
                "not settled after the quiet period: {} orders, {} ambiguous",
                self.engine.orders_count(),
                self.engine.ambiguous_count()
            ));
        }
        for (market, coin) in COINS.iter().enumerate() {
            let exposure = self.engine.exposure(market as u16);
            let held = exposure.expect("a settled engine is running").position.raw();
            let truth = self.model.positions[market];
            if held != truth {
                return Err(format!("{coin}: the engine holds {held}, the exchange {truth}"));
            }
        }
        for (cloid, total) in &self.filled_events {
            let found = self.model.by_cloid.get(cloid);
            let truth = found.map_or(0, |&at| {
                let order = &self.model.orders[at];
                order.sz_orig - order.sz_open
            });
            if *total > truth {
                return Err(format!("{cloid:?}: reported {total} filled, the exchange {truth}"));
            }
        }
        for order in &self.model.orders {
            if order.word == Word::Open && self.engine.order_view(order.cloid).is_none() {
                return Err(format!("{:?} is open and the engine has lost it", order.cloid));
            }
        }
        Ok(())
    }
}

fn request_weight(body: &[u8], endpoint: Endpoint, tokens: &mut [Token]) -> u64 {
    let Ok(document) = parse(body, tokens) else {
        return 1;
    };
    let root = document.root();
    match endpoint {
        Endpoint::Exchange => {
            let action = root.get("action");
            let orders = action.and_then(|value| value.get("orders"));
            let cancels = action.and_then(|value| value.get("cancels"));
            let batch = orders.or(cancels).map_or(0, |list| u64::from(list.count()));
            1 + batch / 40
        }
        Endpoint::Info => match root.get("type").and_then(hypertiger::json::Value::str) {
            Some("orderStatus" | "clearinghouseState") => 2,
            _ => 20,
        },
    }
}

/// The worst cases the oracle holds the engine to. `all` counts every order that could fill, and
/// `gated` leaves out reduce-only orders, which cannot grow a position.
#[derive(Clone, Copy)]
struct Possible {
    long_all: i128,
    short_all: i128,
    long_gated: i128,
    short_gated: i128,
}

impl Possible {
    fn add(&mut self, side: Side, size: i128, reduce_only: bool) {
        if side.is_buy() {
            self.long_all += size;
            if !reduce_only {
                self.long_gated += size;
            }
        } else {
            self.short_all -= size;
            if !reduce_only {
                self.short_gated -= size;
            }
        }
    }
}

fn client_clock(now_ms: u64, skew_ms: i64) -> u64 {
    now_ms.saturating_add_signed(skew_ms)
}

/// Runs one seed, turning an engine assertion into a reported failure.
#[must_use]
pub fn run_seed(seed: u64, durations: Durations) -> Outcome {
    let result = std::panic::catch_unwind(|| World::new(seed).run(durations));
    match result {
        Ok(outcome) => outcome,
        Err(panic) => {
            let reason = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            Outcome {
                seed,
                violation: Some(format!("panic: {reason}")),
                trace_digest: 0,
                coverage: Coverage::default(),
            }
        }
    }
}
