//! The order engine: at-most-once order placement with a risk bound that holds under every network
//! outcome.
//!
//! It is sans-IO. It never reads a clock, never touches a socket and never spawns anything; a
//! driver feeds it time and bytes and takes bytes back. That is what lets `hypertiger-vopr` run the
//! same code through years of failure in seconds, and it follows TigerStyle's rule against acting
//! directly on external events: `on_*` only records what happened, and work leaves at the engine's
//! own pace through `poll_transmit`, in batches.
//!
//! # The problem
//!
//! A request that times out has one of three fates: it never arrived, it landed and the reply was
//! lost, or it is still on its way and will land later. Retrying risks two orders; assuming it
//! failed understates risk. Every client library leaves this to its caller.
//!
//! # The idea
//!
//! An order carries `expiresAfter`, so the third fate has a deadline. Past it, the outcome can be
//! settled for certain by asking about the client's own order id. Until then the engine does not
//! guess: an ambiguous order counts as live, in full, against the risk limit. It is never resent.
//!
//! - `long_bound` and `short_bound` are worst cases across every world the network could have left
//!   behind, and the simulator's oracle holds them against the exchange's real state at every step.
//! - Every process start is a recovery. There is no durable state to corrupt, because the exchange
//!   is the log: the engine waits out the longest expiry a previous life could have used, then
//!   adopts what the exchange says is open.
//!
//! See `docs/DESIGN.md` for the assumptions this rests on, and the one that is not yet verified.

use crate::action::{
    Action, BATCH_SIZE_MAX, CancelCloidWire, Envelope, Grouping, L1Options, OrderKind, OrderRef,
    OrderWire, Tif, sign_l1,
};
use crate::decimal::{Px, Sz, Szi};
use crate::feed::{
    CancelStatus, Channel, ExchangeReply, Fill, Message, OrderStatusReply, OrderUpdate, OrderView,
    PlaceStatus, StatusClass,
};
use crate::info::Info;
use crate::json::{Kind, Value};
use crate::limits::{TokenBucket, exchange_weight};
use crate::market::{Asset, AssetKind, GridError, Market};
use crate::nonce::NonceSource;
use crate::signing::Signer;
use crate::sink::{JsonSink, Walk};
use crate::types::{Address, Cloid, Coin, Network, Side};

/// Requests in flight at once. Well under the 100 posts the exchange allows, since each batch can
/// carry 39 orders and the budget would run out first.
pub const REQUESTS_MAX: usize = 32;
/// Fills remembered for deduplication. A `userFills` snapshot replays up to 2,000, so this must
/// hold two of them: a reconnect overlapping a reconcile must not double-count a position.
pub const FILLS_SEEN_MAX: usize = 4096;
/// The most fills read from one list. The exchange returns at most 2,000.
pub const FILLS_LIST_MAX: usize = 2048;
/// Events held for the caller. Filling up is a caller bug, and it stops new orders rather than
/// lose a fact about money.
pub const EVENTS_MAX: usize = 2048;
/// Status queries sent per tick, so that recovering from a long outage is a trickle, not a burst.
pub const STATUS_QUERIES_PER_TICK: u32 = 8;
/// The largest request body the engine writes: 39 orders at about 170 bytes each, and the envelope.
pub const REQUEST_SIZE_MAX: usize = 16 * 1024;
/// How many times to ask for a finished order's fills before returning a slot that no position read
/// can vouch for, on a market `clearinghouseState` does not cover.
const FILLS_RETRIES_UNTRACKED_MAX: u32 = 8;
/// Fills further ahead of the local clock than this cannot be real. One taken at face value would
/// freeze position tracking, because every real fill after it would look older.
const FILL_AHEAD_MS_MAX: u64 = 60_000;
/// Client ids remembered after their orders finish, so that a reused one is refused. Bounded, so a
/// caller still has to choose ids that do not repeat; see `Intent::cloid`.
pub const CLOIDS_SEEN_MAX: usize = 4096;
/// The most weight one `userFills` reply can add: 2,000 fills at one per 20.
pub const WEIGHT_FILLS_SURCHARGE_MAX: u32 = 100;
/// How many times a cancel's retry interval doubles.
const CANCEL_BACKOFF_DOUBLINGS_MAX: u32 = 6;
/// Weight of one `orderStatus`.
const WEIGHT_STATUS: u32 = 2;

const _: () = assert!(FILLS_SEEN_MAX >= 2 * 2000);
const _: () = assert!(CLOIDS_SEEN_MAX > 0);
const _: () = assert!(WEIGHT_FILLS_SURCHARGE_MAX == 2000 / 20);
const _: () = assert!(FILLS_LIST_MAX >= 2000);
const _: () = assert!(REQUESTS_MAX <= crate::limits::WS_POSTS_INFLIGHT_MAX as usize);
const _: () = assert!(BATCH_SIZE_MAX * 200 + 512 <= REQUEST_SIZE_MAX);
const _: () = assert!(BATCH_SIZE_MAX < u8::MAX as usize);

/// Tuning, with no defaults: each of these changes what can happen to money, so each is written
/// where the engine is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    pub network: Network,
    /// The account whose orders and positions these are: the one that actually holds them. For an
    /// API wallet that is its master account, never the wallet, and for a vault or sub-account it
    /// is that address. Asking about any other account answers `unknownOid` for orders that are
    /// really there, which would turn into a false proof that they never landed.
    pub user: Address,
    /// The vault or sub-account traded for, if any. It must equal `user`.
    pub vault: Option<Address>,
    /// How often batches leave. The docs suggest 100 ms.
    pub tick_ms: u32,
    /// How long an order may take to land, which becomes `expiresAfter`. The longer it is, the
    /// longer an ambiguity lasts; the shorter, the more actions die stale at 5x the rate-limit
    /// cost.
    pub order_ttl_ms: u32,
    /// How long past `expiresAfter` before "unknown" is believed. It must exceed the clock skew to
    /// block time plus how far an API server's reads can lag. See `docs/DESIGN.md`.
    pub settle_margin_ms: u32,
    /// How long a request waits for its reply before its outcome is called ambiguous.
    pub request_timeout_ms: u32,
    /// How long an order may wait in the queue. A quote that is seconds old is not the quote the
    /// caller meant, so it dies instead of being sent.
    pub queue_ms_max: u32,
    /// The wait between status queries on an order in doubt.
    pub status_poll_ms: u32,
    /// How often open orders and positions are compared with the exchange's.
    pub reconcile_ms: u32,
    /// How often to ask again for a finished order's missing fills.
    pub fills_wait_ms: u32,
    /// Sustained REST weight per minute. With `weight_burst` and `WEIGHT_FILLS_SURCHARGE_MAX`, it
    /// must not exceed 1200.
    pub weight_per_minute: u32,
    pub weight_burst: u32,
    /// Weight kept back from everything but cancels, so that a client out of budget can still get
    /// out of the market.
    pub weight_cancel_reserve: u32,
    /// Whether cancels use the `f` flag. Engine orders are never triggers, so it is safe here.
    pub fast_cancel: bool,
}

impl EngineConfig {
    fn assert_valid(&self) {
        assert!(self.tick_ms > 0);
        assert!(self.order_ttl_ms > 0);
        assert!(self.settle_margin_ms > 0);
        assert!(self.request_timeout_ms > 0);
        assert!(self.status_poll_ms > 0);
        assert!(self.reconcile_ms > 0);
        assert!(self.fills_wait_ms > 0);
        assert!(self.weight_per_minute > 0);
        assert!(self.weight_burst > self.weight_cancel_reserve);
        assert!(
            self.weight_per_minute + self.weight_burst + WEIGHT_FILLS_SURCHARGE_MAX
                <= crate::limits::IP_WEIGHT_PER_MINUTE,
            "a bucket can reach rate + burst in one sliding minute, and a fill list can add its \
             surcharge before the bucket hears about it, so that is what must fit"
        );
        if let Some(vault) = self.vault {
            assert!(self.user == vault, "account reads must ask about the account that holds them");
        }
    }

    /// The longest an action this config signed can still land, measured from when it was signed.
    #[must_use]
    pub const fn quarantine_ms(&self) -> u64 {
        self.order_ttl_ms as u64 + self.settle_margin_ms as u64
    }
}

/// A market the engine may trade, and how far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketConfig {
    pub coin: Coin,
    pub market: Market,
    pub long_sz_max: Sz,
    pub short_sz_max: Sz,
}

/// What the caller wants to happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    /// Chosen by the caller, and never reused, across restarts too. The engine refuses one it still
    /// remembers, but its memory is bounded and does not survive a process, and what the exchange
    /// does with a repeat is unknown. Use a `session` that differs per process start, with
    /// `Cloid::from_parts`, and a sequence that only rises.
    pub cloid: Cloid,
    pub market_index: u16,
    pub side: Side,
    pub tif: Tif,
    pub px: Px,
    pub sz: Sz,
    pub reduce_only: bool,
}

/// Why an intent was refused. Nothing was sent, and nothing will be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// Waiting out a previous life's orders. Unknown exposure may still land.
    Recovering,
    /// The engine stopped itself; see `Event::Halted`.
    Halted,
    UnknownMarket,
    DuplicateCloid,
    SlotsFull,
    Grid(GridError),
    /// The worst case, with this order, would pass the configured limit.
    RiskLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelError {
    NotFound,
    /// Adopted from the exchange with no client id. The engine only cancels by client id.
    NoCloid,
    /// Adopted from the exchange as a TP/SL. The engine manages limit orders only, and its fast
    /// cancel cannot reach one; cancel it with `Action::Cancel` and `fast: false`.
    Trigger,
}

/// Why an order is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// All of it filled.
    Filled,
    /// Over with this much filled: canceled, an IOC's remainder, or killed by the exchange.
    Dead,
    /// The exchange refused it, or refused the action carrying it.
    Rejected,
    /// Rate-limited at the door, which is a definite "not processed".
    Throttled,
    /// Past its expiry, and the exchange says it never saw it. Proven not to have landed.
    NeverLanded,
    /// Canceled by the caller before it was ever sent.
    CanceledInQueue,
    /// Waited in the queue past `queue_ms_max` and was dropped unsent.
    ExpiredInQueue,
}

/// A fact the caller should know. Events are conserved: none is dropped while the engine runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The order is on the book.
    Resting { cloid: Cloid, oid: u64 },
    /// New exposure, deduplicated. `cloid` is `None` for an order not placed with one.
    Filled { cloid: Option<Cloid>, market_index: u16, side: Side, px: Px, sz: Sz, time_ms: u64 },
    /// The order is over and its slot is free. `sz_filled` counts the fills seen for it.
    Finished { cloid: Option<Cloid>, sz_filled: Sz, finish: Finish },
    /// A request's fate is unknown, and these orders now count as live until it is settled.
    Ambiguous { cloid: Cloid },
    /// The exchange reports a status this version cannot read. The order stays live.
    StatusUnread { cloid: Cloid },
    /// A finished order's fills never came. A position read proved they were already counted, or,
    /// on a market no read covers, the slot had to be returned after repeated asking.
    FillsMissing { cloid: Option<Cloid>, sz_expected: Sz, sz_seen: Sz },
    /// The exchange's position differed from the fills seen, and replaced them.
    PositionCorrected { market_index: u16, from: Szi, to: Szi },
    /// The exchange reported an open order this engine did not know.
    Adopted { cloid: Option<Cloid>, oid: u64 },
    /// Quarantine is over and the exchange's state is loaded. Orders are accepted from here on.
    Recovered,
    /// The engine will accept no more orders until restarted. Cancels still go out.
    Halted { reason: HaltReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    /// More open orders on the exchange than slots, so the bound would be a lie.
    SlotsExhausted,
    /// The caller stopped draining events.
    EventsFull,
    /// The exchange's clock disagrees with this one by more than the margin, so "past the deadline"
    /// can no longer be believed.
    ClockSkew,
    /// The driver saw the local clock step. A restart takes a fresh reading and waits out
    /// quarantine.
    ClockStep,
}

/// A request for the driver to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transmit {
    /// Hand this back with the reply. Doubles as the WebSocket post `id`.
    pub request_id: u64,
    pub endpoint: Endpoint,
    /// Bytes written to the target.
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Exchange,
    Info,
}

/// What became of a request, as the driver saw it.
#[derive(Clone, Copy, Debug)]
pub enum Reply<'s, 't> {
    /// A parseable reply. For a WebSocket post, the `payload`; for info, its `data`.
    Value(Value<'s, 't>),
    /// The server's answer could not be read, or never came. The action may have landed.
    Failed,
    /// HTTP 429: refused at the door, so it certainly did not land.
    Throttled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Free,
    /// Accepted, and not yet on the wire. Cancellable with no trace.
    Queued,
    /// On the wire, reply pending.
    Inflight,
    /// The request failed without a verdict. Counted in full until settled.
    Ambiguous,
    /// On the book, or known to have been.
    Live,
    /// Over, with `sz_final` filled in total, waiting for those fills to be seen.
    Settling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancelState {
    None,
    /// Wanted, and sticky: an order that lands after its cancel still gets canceled.
    Wanted,
    Sent,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    state: State,
    cancel: CancelState,
    /// `None` only for an order adopted from the exchange that was placed without one.
    cloid: Option<Cloid>,
    /// Zero until the exchange names it.
    oid: u64,
    market_index: u16,
    side: Side,
    tif: Tif,
    reduce_only: bool,
    px: Px,
    sz: Sz,
    sz_filled: Sz,
    sz_final: Sz,
    /// What the slot is waiting on: `queued_ms` when `Queued`, `expires_ms` when in flight or
    /// ambiguous, the fills deadline when `Settling`.
    deadline_ms: u64,
    /// When this slot next deserves a status query. `u64::MAX` when it does not.
    poll_ms: u64,
    cancel_not_before_ms: u64,
    /// How many times this cancel has been refused or lost, so that retries back off.
    cancel_retries: u32,
    /// Adopted as a TP/SL, so it is counted but not managed.
    is_trigger: bool,
    /// When the order was learned to be over. Only a position read later than this by the whole
    /// margin is certain to include its fills.
    settled_ms: u64,
    /// How many times its fills have been asked for since.
    fills_retries: u32,
    /// For an adopted order: fills up to this time may already be in the remaining size it was
    /// adopted with, so they do not reduce it again. Skipping one overstates what is open, which is
    /// the safe direction. Counting one twice would understate it.
    credit_floor_ms: u64,
    finish: Finish,
    /// The request carrying this order, while `Inflight`.
    request_id: u64,
    /// Counts this slot's occupants. A request names a slot and a generation together, because a
    /// late reply must never reach the different order that has since moved into the same slot.
    generation: u32,
}

const SLOT_FREE: Slot = Slot {
    state: State::Free,
    cancel: CancelState::None,
    cloid: None,
    oid: 0,
    market_index: 0,
    side: Side::Buy,
    tif: Tif::Gtc,
    reduce_only: false,
    px: Px::ZERO,
    sz: Sz::ZERO,
    sz_filled: Sz::ZERO,
    sz_final: Sz::ZERO,
    deadline_ms: 0,
    poll_ms: u64::MAX,
    cancel_not_before_ms: 0,
    cancel_retries: 0,
    is_trigger: false,
    settled_ms: 0,
    fills_retries: 0,
    credit_floor_ms: 0,
    finish: Finish::Dead,
    request_id: 0,
    generation: 0,
};

const _: () = assert!(size_of::<Slot>() <= 192);

/// A slot as a particular order held it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlotRef {
    index: u32,
    generation: u32,
}

const SLOT_REF_NONE: SlotRef = SlotRef { index: 0, generation: 0 };

impl Slot {
    /// What this order can still add to the position, in the world most unkind to the bound.
    fn open_sz(&self) -> Sz {
        match self.state {
            State::Free => Sz::ZERO,
            State::Queued | State::Inflight | State::Ambiguous | State::Live => {
                self.sz.saturating_sub(self.sz_filled)
            }
            State::Settling => self.sz_final.saturating_sub(self.sz_filled),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestKind {
    Place,
    Cancel,
    Status,
    OpenOrders,
    Positions,
    Fills,
}

#[derive(Clone, Copy, Debug)]
struct Request {
    active: bool,
    id: u64,
    kind: RequestKind,
    deadline_ms: u64,
    slots: [SlotRef; BATCH_SIZE_MAX],
    slots_count: u8,
}

const REQUEST_FREE: Request = Request {
    active: false,
    id: 0,
    kind: RequestKind::Place,
    deadline_ms: 0,
    slots: [SLOT_REF_NONE; BATCH_SIZE_MAX],
    slots_count: 0,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// A previous life's actions may still land until this time.
    Quarantine {
        until_ms: u64,
    },
    /// Loading what the exchange says is open.
    Snapshot,
    Running,
}

/// Whether a snapshot is owed and whether one is on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Idle,
    Wanted,
    Inflight,
}

#[derive(Clone, Copy, Debug)]
struct MarketState {
    config: MarketConfig,
    position: Szi,
    /// The newest fill applied to `position`. Per market: anchoring on `startPosition` is only
    /// meaningful against fills of the same coin.
    fill_time_last_ms: u64,
}

const MARKET_EMPTY: Option<MarketState> = None;

/// The fill identity. The docs give `(time, coin, tid)` as unique; adding `oid` keeps a
/// self-trade's two sides apart, and each engine is scoped to one account's markets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FillKey {
    time_ms: u64,
    tid: u64,
    oid: u64,
}

const FILL_KEY_EMPTY: FillKey = FillKey { time_ms: 0, tid: 0, oid: 0 };

/// How a list of fills is sorted, which depends on where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillOrder {
    NewestFirst,
    OldestFirst,
}

/// Which of the two order lanes a batch is. The docs recommend batching ALO apart from the rest,
/// because validators prioritize ALO-only batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Alo,
    Taker,
}

/// The engine. `ORDERS` is how many orders may be alive at once and `MARKETS` how many markets it
/// trades; both are fixed here, and the struct holds all its memory inline.
#[derive(Debug)]
pub struct Engine<const ORDERS: usize, const MARKETS: usize> {
    config: EngineConfig,
    phase: Phase,
    now_ms: u64,
    next_tick_ms: u64,
    lanes_ready: LanesReady,
    status_budget: u32,
    throttled_until_ms: u64,
    halted: Option<HaltReason>,

    slots: [Slot; ORDERS],
    requests: [Request; REQUESTS_MAX],
    request_id_next: u64,
    markets: [Option<MarketState>; MARKETS],
    markets_count: u16,

    bucket: TokenBucket,
    nonces: NonceSource,

    fills_seen: [FillKey; FILLS_SEEN_MAX],
    fills_seen_next: usize,
    /// Fills at or before this time predate this process: they anchor the position, but they are
    /// not news for the caller and they were not this process's orders.
    fills_floor_ms: u64,

    need_open_orders: Need,
    need_positions: Need,
    need_fills: Need,
    positions_not_before_ms: u64,
    /// A read that failed or could not be understood waits this long, so that a schema change
    /// cannot spend the whole budget on asking again.
    reads_not_before_ms: u64,
    reconcile_next_ms: u64,
    /// Client ids of finished orders.
    cloids_seen: [Option<Cloid>; CLOIDS_SEEN_MAX],
    cloids_seen_next: usize,
    /// Fields and replies from the exchange that could not be read and were set aside.
    unreadable_count: u64,

    events: [Option<Event>; EVENTS_MAX],
    event_head: usize,
    events_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct LanesReady {
    cancel: bool,
    alo: bool,
    taker: bool,
}

/// Open size on one market, split by side and by whether it can only reduce.
#[derive(Clone, Copy, Debug)]
struct Working {
    buys: i128,
    sells: i128,
    buys_reducing: i128,
    sells_reducing: i128,
}

/// The engine's knowledge of one market: what it can answer without the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exposure {
    pub position: Szi,
    /// The most long this account can be, whichever way every open question resolves.
    pub long_bound: Szi,
    /// The most short it can be, as a negative number or zero, likewise.
    pub short_bound: Szi,
}

impl<const ORDERS: usize, const MARKETS: usize> Engine<ORDERS, MARKETS> {
    const LAYOUT: () = {
        assert!(ORDERS > 0);
        assert!(ORDERS <= u32::MAX as usize);
        assert!(MARKETS > 0);
        assert!(MARKETS <= u16::MAX as usize);
    };

    /// A new engine, which begins by waiting out whatever a previous process may have left in
    /// flight. There is no separate clean start: crash-only software has one way in.
    ///
    /// The large arrays pass through the stack once, here at startup, because building them in
    /// place would take `unsafe`. Call this on a thread with a normal stack, and box the result.
    #[must_use]
    #[allow(clippy::large_stack_arrays)]
    pub fn new(config: EngineConfig, markets: &[MarketConfig], now_ms: u64) -> Self {
        let () = Self::LAYOUT;
        config.assert_valid();
        assert_ne!(markets, []);
        assert!(markets.len() <= MARKETS);

        let mut states = [MARKET_EMPTY; MARKETS];
        for (index, market) in markets.iter().enumerate() {
            states[index] =
                Some(MarketState { config: *market, position: Szi::ZERO, fill_time_last_ms: 0 });
        }
        Engine {
            config,
            phase: Phase::Quarantine { until_ms: now_ms.saturating_add(config.quarantine_ms()) },
            now_ms,
            next_tick_ms: now_ms,
            lanes_ready: LanesReady { cancel: false, alo: false, taker: false },
            status_budget: 0,
            throttled_until_ms: 0,
            halted: None,
            slots: [SLOT_FREE; ORDERS],
            requests: [REQUEST_FREE; REQUESTS_MAX],
            request_id_next: 1,
            markets: states,
            markets_count: markets.len() as u16,
            bucket: TokenBucket::new_empty(config.weight_per_minute, config.weight_burst, now_ms),
            nonces: NonceSource::new(now_ms),
            fills_seen: [FILL_KEY_EMPTY; FILLS_SEEN_MAX],
            fills_seen_next: 0,
            fills_floor_ms: now_ms.saturating_sub(u64::from(config.settle_margin_ms)),
            need_open_orders: Need::Idle,
            need_positions: Need::Idle,
            need_fills: Need::Idle,
            positions_not_before_ms: 0,
            reads_not_before_ms: 0,
            reconcile_next_ms: 0,
            cloids_seen: [None; CLOIDS_SEEN_MAX],
            cloids_seen_next: 0,
            unreadable_count: 0,
            events: [None; EVENTS_MAX],
            event_head: 0,
            events_count: 0,
        }
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running)
    }

    #[must_use]
    pub const fn halted(&self) -> Option<HaltReason> {
        self.halted
    }

    fn market(&self, market_index: u16) -> &MarketState {
        assert!(market_index < self.markets_count);
        match &self.markets[usize::from(market_index)] {
            Some(state) => state,
            None => panic!("every index below markets_count holds a market"),
        }
    }

    fn market_mut(&mut self, market_index: u16) -> &mut MarketState {
        assert!(market_index < self.markets_count);
        match &mut self.markets[usize::from(market_index)] {
            Some(state) => state,
            None => panic!("every index below markets_count holds a market"),
        }
    }

    fn market_index_of(&self, coin: &Coin) -> Option<u16> {
        assert!(self.markets_count > 0);
        (0..self.markets_count).find(|&index| self.market(index).config.coin == *coin)
    }

    /// The worst cases for one market, across every open question. `None` until recovery finishes:
    /// before then the exchange may hold anything, and zeros would be a lie.
    #[must_use]
    pub fn exposure(&self, market_index: u16) -> Option<Exposure> {
        assert!(market_index < self.markets_count);
        if self.is_running() { Some(self.exposure_known(market_index)) } else { None }
    }

    fn exposure_known(&self, market_index: u16) -> Exposure {
        let position = self.market(market_index).position;
        let mut long_bound = position.raw();
        let mut short_bound = position.raw();
        for slot in &self.slots {
            // Reduce-only orders count too. They cannot flip a position, but until their fills are
            // seen the held position is stale by their size, and the bound has to cover that.
            if slot.state != State::Free && slot.market_index == market_index {
                let open = i128::try_from(slot.open_sz().raw()).unwrap_or(i128::MAX);
                if slot.side.is_buy() {
                    long_bound = long_bound.saturating_add(open);
                } else {
                    short_bound = short_bound.saturating_sub(open);
                }
            }
        }
        assert!(long_bound >= position.raw());
        assert!(short_bound <= position.raw());
        assert!(long_bound >= short_bound);
        Exposure {
            position,
            long_bound: Szi::from_raw(long_bound),
            short_bound: Szi::from_raw(short_bound),
        }
    }

    /// Takes an intent. Refusals here cost nothing: no bytes have moved.
    pub fn submit(&mut self, intent: &Intent, now_ms: u64) -> Result<(), SubmitError> {
        self.advance_time(now_ms);
        match self.phase {
            Phase::Running => {}
            Phase::Quarantine { .. } | Phase::Snapshot => return Err(SubmitError::Recovering),
        }
        if self.halted.is_some() {
            return Err(SubmitError::Halted);
        }
        if intent.market_index >= self.markets_count {
            return Err(SubmitError::UnknownMarket);
        }
        let config = self.market(intent.market_index).config;
        config
            .market
            .order_check(intent.px, intent.sz, intent.reduce_only)
            .map_err(SubmitError::Grid)?;
        let finished_before = self.cloids_seen.contains(&Some(intent.cloid));
        if finished_before || self.slot_of_cloid(intent.cloid).is_some() {
            return Err(SubmitError::DuplicateCloid);
        }
        let Some(index) = self.slots.iter().position(|slot| slot.state == State::Free) else {
            return Err(SubmitError::SlotsFull);
        };
        self.check_risk(intent, &config)?;

        self.slots[index] = Slot {
            state: State::Queued,
            cloid: Some(intent.cloid),
            market_index: intent.market_index,
            side: intent.side,
            tif: intent.tif,
            reduce_only: intent.reduce_only,
            px: intent.px,
            sz: intent.sz,
            deadline_ms: self.now_ms,
            generation: self.slots[index].generation,
            ..SLOT_FREE
        };
        assert!(self.slots[index].state == State::Queued);
        assert!(self.slot_of_cloid(intent.cloid) == Some(index));
        Ok(())
    }

    /// The gate. A worst case is reached in two moves: reduce-only orders first take the position
    /// as far toward flat as they can, then every ordinary order fills. That first move is easy to
    /// miss. Long 1.5 with 5 of sells working looks like a worst case of -3.5, but a reduce-only
    /// sell closing the long leaves all 5 naked. The simulator found this; a reduce-only order is
    /// gated too, because it can remove the hedge that made other orders acceptable.
    fn check_risk(&self, intent: &Intent, config: &MarketConfig) -> Result<(), SubmitError> {
        assert!(intent.market_index < self.markets_count);
        assert!(config.coin == self.market(intent.market_index).config.coin);
        let size = i128::try_from(intent.sz.raw()).map_err(|_| SubmitError::RiskLimit)?;
        let mut working = self.working_sizes(intent.market_index);
        match (intent.side, intent.reduce_only) {
            (Side::Buy, false) => working.buys = working.buys.saturating_add(size),
            (Side::Buy, true) => working.buys_reducing = working.buys_reducing.saturating_add(size),
            (Side::Sell, false) => working.sells = working.sells.saturating_add(size),
            (Side::Sell, true) => {
                working.sells_reducing = working.sells_reducing.saturating_add(size);
            }
        }
        let position = self.market(intent.market_index).position.raw();
        if intent.side.is_buy() {
            let flat_or_above = position.saturating_add(working.buys_reducing).min(position.max(0));
            let worst = flat_or_above.saturating_add(working.buys);
            let limit = i128::try_from(config.long_sz_max.raw()).unwrap_or(i128::MAX);
            if worst > limit { Err(SubmitError::RiskLimit) } else { Ok(()) }
        } else {
            let flat_or_below =
                position.saturating_sub(working.sells_reducing).max(position.min(0));
            let worst = flat_or_below.saturating_sub(working.sells);
            let limit = i128::try_from(config.short_sz_max.raw()).unwrap_or(i128::MAX);
            if worst < -limit { Err(SubmitError::RiskLimit) } else { Ok(()) }
        }
    }

    fn working_sizes(&self, market_index: u16) -> Working {
        let mut working = Working { buys: 0, sells: 0, buys_reducing: 0, sells_reducing: 0 };
        for slot in &self.slots {
            if slot.state != State::Free && slot.market_index == market_index {
                let open = i128::try_from(slot.open_sz().raw()).unwrap_or(i128::MAX);
                let total = match (slot.side, slot.reduce_only) {
                    (Side::Buy, false) => &mut working.buys,
                    (Side::Buy, true) => &mut working.buys_reducing,
                    (Side::Sell, false) => &mut working.sells,
                    (Side::Sell, true) => &mut working.sells_reducing,
                };
                *total = total.saturating_add(open);
            }
        }
        assert!(working.buys >= 0);
        assert!(working.sells >= 0);
        assert!(working.buys_reducing >= 0);
        assert!(working.sells_reducing >= 0);
        working
    }

    /// Asks for an order to be canceled. Sticky: the engine keeps trying until the order is over,
    /// including when the order turns out to have landed after its cancel did.
    pub fn cancel(&mut self, cloid: Cloid, now_ms: u64) -> Result<(), CancelError> {
        self.advance_time(now_ms);
        let Some(index) = self.slot_of_cloid(cloid) else {
            return Err(CancelError::NotFound);
        };
        if self.slots[index].is_trigger {
            return Err(CancelError::Trigger);
        }
        match self.slots[index].state {
            State::Free => panic!("slot_of_cloid never returns a free slot"),
            State::Queued => self.finish_slot(index, Finish::CanceledInQueue),
            State::Settling => {}
            State::Inflight | State::Ambiguous | State::Live => {
                if self.slots[index].cancel == CancelState::None {
                    self.slots[index].cancel = CancelState::Wanted;
                    self.slots[index].cancel_not_before_ms = self.now_ms;
                }
                assert!(self.slots[index].cancel != CancelState::None);
            }
        }
        assert!(self.slot_of_cloid(cloid).is_none_or(|at| self.slots[at].state != State::Queued));
        Ok(())
    }

    /// Tells the engine a feed connection dropped and came back, so anything could have been
    /// missed. Everything gets compared with the exchange.
    pub fn on_feed_reconnected(&mut self, now_ms: u64) {
        self.advance_time(now_ms);
        self.want_reconcile();
        // Fills are the expensive read, at up to 120 weight, so only a gap in the feed earns one.
        if self.is_running() {
            self.need_fills = want(self.need_fills);
        }
        assert!(!self.is_running() || self.need_positions != Need::Idle);
        assert!(!self.is_running() || self.need_fills != Need::Idle);
    }

    fn want_reconcile(&mut self) {
        if self.is_running() {
            self.need_open_orders = want(self.need_open_orders);
            self.need_positions = want(self.need_positions);
            self.positions_not_before_ms = self.now_ms;
            assert!(self.need_open_orders != Need::Idle);
            assert!(self.positions_not_before_ms <= self.now_ms);
        }
    }

    /// Stops new orders because the driver saw the clock step. Cancels still go out.
    pub fn halt_for_clock_step(&mut self) {
        self.halt(HaltReason::ClockStep);
        assert!(self.halted.is_some());
    }

    /// Exchange data set aside as unreadable since the start. A number that climbs means the wire
    /// has changed shape, and the engine is seeing less than the exchange is saying.
    #[must_use]
    pub const fn unreadable_count(&self) -> u64 {
        self.unreadable_count
    }

    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Advances time: timeouts, expiries and the batch clock. Call it at least once per tick.
    pub fn tick(&mut self, now_ms: u64) {
        self.advance_time(now_ms);
        self.tick_phase();
        self.tick_requests();
        self.tick_slots();
        if self.now_ms >= self.next_tick_ms {
            self.next_tick_ms = self.now_ms + u64::from(self.config.tick_ms);
            self.lanes_ready = LanesReady { cancel: true, alo: true, taker: true };
            self.status_budget = STATUS_QUERIES_PER_TICK;
        }
        if self.is_running() && self.now_ms >= self.reconcile_next_ms {
            self.reconcile_next_ms = self.now_ms + u64::from(self.config.reconcile_ms);
            self.want_reconcile();
        }
        assert!(self.next_tick_ms > self.now_ms);
        assert!(self.status_budget <= STATUS_QUERIES_PER_TICK);
        self.assert_invariants();
    }

    fn advance_time(&mut self, now_ms: u64) {
        // A wall clock may step backward. Time here never does, and a late step only delays things.
        let before_ms = self.now_ms;
        if now_ms > self.now_ms {
            self.now_ms = now_ms;
        }
        assert!(self.now_ms >= before_ms);
        assert!(self.now_ms >= now_ms);
    }

    fn tick_phase(&mut self) {
        if let Phase::Quarantine { until_ms } = self.phase {
            if self.now_ms >= until_ms {
                self.phase = Phase::Snapshot;
                self.need_open_orders = Need::Wanted;
                assert!(self.need_positions == Need::Idle, "positions wait for the order list");
                assert!(self.orders_count() == 0, "nothing was accepted during quarantine");
            }
        }
    }

    fn tick_requests(&mut self) {
        for index in 0..REQUESTS_MAX {
            let request = self.requests[index];
            if request.active && self.now_ms >= request.deadline_ms {
                self.requests[index] = REQUEST_FREE;
                self.request_failed(&request);
            }
        }
        let overdue = self.requests.iter().any(|r| r.active && self.now_ms >= r.deadline_ms);
        assert!(!overdue, "every overdue request was just failed");
    }

    fn tick_slots(&mut self) {
        for index in 0..ORDERS {
            let slot = self.slots[index];
            match slot.state {
                State::Queued => {
                    let age_ms = self.now_ms.saturating_sub(slot.deadline_ms);
                    if age_ms >= u64::from(self.config.queue_ms_max) {
                        self.finish_slot(index, Finish::ExpiredInQueue);
                    }
                }
                State::Settling if self.now_ms >= slot.deadline_ms => {
                    self.tick_settling(index);
                }
                _ => {}
            }
        }
        let queue_ms_max = u64::from(self.config.queue_ms_max);
        let stale = self.slots.iter().any(|slot| {
            slot.state == State::Queued
                && self.now_ms.saturating_sub(slot.deadline_ms) >= queue_ms_max
        });
        assert!(!stale, "every stale intent was just dropped");
    }

    /// A finished order's fills are late. Its unseen size stays in the bound however long that
    /// takes: freeing it on a timer would understate risk until the next check, and the bound is a
    /// promise for every moment. So this only asks again. See `close_overdue`, which ends the wait.
    fn tick_settling(&mut self, index: usize) {
        assert!(self.slots[index].sz_final >= self.slots[index].sz_filled);
        assert_eq!(self.slots[index].state, State::Settling);
        let wait_ms = u64::from(self.config.fills_wait_ms);
        let slot = &mut self.slots[index];
        slot.fills_retries = slot.fills_retries.saturating_add(1);
        slot.deadline_ms = self.now_ms + wait_ms;
        let retries = slot.fills_retries;
        let market_index = slot.market_index;
        self.need_fills = want(self.need_fills);
        self.need_positions = want(self.need_positions);
        // No position read covers these markets, so nothing can prove the size arrived. Eventually
        // the slot has to be returned, and the event says so.
        if !self.tracks_position(market_index) && retries >= FILLS_RETRIES_UNTRACKED_MAX {
            self.close_missing(index);
        }
    }

    fn close_missing(&mut self, index: usize) {
        let slot = self.slots[index];
        assert_eq!(slot.state, State::Settling);
        self.push_event(Event::FillsMissing {
            cloid: slot.cloid,
            sz_expected: slot.sz_final,
            sz_seen: slot.sz_filled,
        });
        self.finish_slot(index, slot.finish);
        assert!(self.slots[index].state == State::Free);
    }

    /// Writes the next request into `target`, if one is due. Call until it returns `None`.
    ///
    /// Nonces are assigned here, at the last moment before the wire, so that they leave in order.
    pub fn poll_transmit<A: Signer>(
        &mut self,
        now_ms: u64,
        signer: &A,
        target: &mut [u8],
    ) -> Option<Transmit> {
        assert!(target.len() >= REQUEST_SIZE_MAX);
        self.advance_time(now_ms);
        assert!(self.status_budget <= STATUS_QUERIES_PER_TICK);
        let request_index = self.requests.iter().position(|request| !request.active)?;

        if self.lanes_ready.cancel {
            self.lanes_ready.cancel = false;
            if let Some(sent) = self.transmit_cancels(request_index, signer, target) {
                return Some(sent);
            }
        }
        if let Some(sent) = self.transmit_status(request_index, target) {
            return Some(sent);
        }
        if let Some(sent) = self.transmit_snapshot(request_index, target) {
            return Some(sent);
        }
        if self.lanes_ready.alo {
            self.lanes_ready.alo = false;
            if let Some(sent) = self.transmit_orders(Lane::Alo, request_index, signer, target) {
                return Some(sent);
            }
        }
        if self.lanes_ready.taker {
            self.lanes_ready.taker = false;
            if let Some(sent) = self.transmit_orders(Lane::Taker, request_index, signer, target) {
                return Some(sent);
            }
        }
        None
    }

    fn occupy_request(&mut self, request_index: usize, kind: RequestKind, slots: &[u32]) -> u64 {
        assert!(!self.requests[request_index].active);
        assert!(slots.len() <= BATCH_SIZE_MAX);
        let id = self.request_id_next;
        self.request_id_next += 1;
        let mut stored = [SLOT_REF_NONE; BATCH_SIZE_MAX];
        for (position, &index) in slots.iter().enumerate() {
            assert_ne!(self.slots[index as usize].state, State::Free);
            stored[position] = SlotRef { index, generation: self.slots[index as usize].generation };
        }
        self.requests[request_index] = Request {
            active: true,
            id,
            kind,
            deadline_ms: self.now_ms + u64::from(self.config.request_timeout_ms),
            slots: stored,
            slots_count: slots.len() as u8,
        };
        id
    }

    fn transmit_orders<A: Signer>(
        &mut self,
        lane: Lane,
        request_index: usize,
        signer: &A,
        target: &mut [u8],
    ) -> Option<Transmit> {
        assert!(!self.requests[request_index].active);

        assert!(target.len() >= REQUEST_SIZE_MAX);
        if self.now_ms < self.throttled_until_ms {
            return None;
        }
        let mut chosen = [0u32; BATCH_SIZE_MAX];
        let mut orders = [PLACEHOLDER_ORDER; BATCH_SIZE_MAX];
        let count = self.choose_orders(lane, &mut chosen, &mut orders);
        if count == 0 {
            return None;
        }
        let weight = exchange_weight(count as u32);
        if !self.bucket.take(self.now_ms, weight, self.config.weight_cancel_reserve) {
            return None;
        }
        let nonce = self.nonces.next(self.now_ms).ok()?;
        let expires_ms = self.now_ms + u64::from(self.config.order_ttl_ms);
        let action =
            Action::Order { orders: &orders[..count], grouping: Grouping::Na, builder: None };
        let size = self.write_signed(&action, nonce, Some(expires_ms), signer, target);

        let request_id = self.occupy_request(request_index, RequestKind::Place, &chosen[..count]);
        for &index in &chosen[..count] {
            let slot = &mut self.slots[index as usize];
            assert_eq!(slot.state, State::Queued);
            slot.state = State::Inflight;
            slot.deadline_ms = expires_ms;
            slot.request_id = request_id;
        }
        assert!(expires_ms > self.now_ms);
        assert!(size <= target.len());
        Some(Transmit { request_id, endpoint: Endpoint::Exchange, size })
    }

    fn choose_orders(
        &self,
        lane: Lane,
        chosen: &mut [u32; BATCH_SIZE_MAX],
        orders: &mut [OrderWire; BATCH_SIZE_MAX],
    ) -> usize {
        let mut count = 0usize;
        for (index, slot) in self.slots.iter().enumerate() {
            if count == BATCH_SIZE_MAX {
                break;
            }
            if slot.state == State::Queued && lane_of(slot.tif) == lane {
                chosen[count] = index as u32;
                orders[count] = OrderWire {
                    asset: self.market(slot.market_index).config.market.asset(),
                    side: slot.side,
                    px: slot.px,
                    sz: slot.sz,
                    reduce_only: slot.reduce_only,
                    kind: OrderKind::Limit(slot.tif),
                    cloid: slot.cloid,
                };
                count += 1;
            }
        }
        assert!(count <= BATCH_SIZE_MAX);
        assert!(chosen[..count].iter().all(|&at| self.slots[at as usize].state == State::Queued));
        count
    }

    fn write_signed<W: Walk, A: Signer>(
        &self,
        action: &W,
        nonce: u64,
        expires_after_ms: Option<u64>,
        signer: &A,
        target: &mut [u8],
    ) -> usize {
        assert!(nonce > self.nonces.previous().saturating_sub(1));

        assert!(target.len() >= REQUEST_SIZE_MAX);
        let options = L1Options {
            network: self.config.network,
            nonce,
            vault: self.config.vault,
            expires_after_ms,
        };
        let signature = sign_l1(action, &options, signer);
        let envelope =
            Envelope { action, nonce, signature, vault: self.config.vault, expires_after_ms };
        write_body(&envelope, target)
    }

    fn transmit_cancels<A: Signer>(
        &mut self,
        request_index: usize,
        signer: &A,
        target: &mut [u8],
    ) -> Option<Transmit> {
        assert!(!self.requests[request_index].active);

        assert!(target.len() >= REQUEST_SIZE_MAX);
        let mut chosen = [0u32; BATCH_SIZE_MAX];
        let mut cancels = [PLACEHOLDER_CANCEL; BATCH_SIZE_MAX];
        let count = self.choose_cancels(&mut chosen, &mut cancels);
        if count == 0 {
            return None;
        }
        // Cancels spend the reserve: the one action that must work when the budget is gone.
        if !self.bucket.take(self.now_ms, exchange_weight(count as u32), 0) {
            self.lanes_ready.cancel = true;
            return None;
        }
        let nonce = self.nonces.next(self.now_ms).ok()?;
        let action =
            Action::CancelByCloid { cancels: &cancels[..count], fast: self.config.fast_cancel };
        // No expiry: a cancel landing late is harmless, and a stale one costs 5x the rate limit.
        let size = self.write_signed(&action, nonce, None, signer, target);

        let request_id = self.occupy_request(request_index, RequestKind::Cancel, &chosen[..count]);
        for &index in &chosen[..count] {
            self.slots[index as usize].cancel = CancelState::Sent;
        }
        assert!(size <= target.len());
        Some(Transmit { request_id, endpoint: Endpoint::Exchange, size })
    }

    fn choose_cancels(
        &self,
        chosen: &mut [u32; BATCH_SIZE_MAX],
        cancels: &mut [CancelCloidWire; BATCH_SIZE_MAX],
    ) -> usize {
        let mut count = 0usize;
        for (index, slot) in self.slots.iter().enumerate() {
            if count == BATCH_SIZE_MAX {
                break;
            }
            let eligible = matches!(slot.state, State::Inflight | State::Ambiguous | State::Live);
            if eligible
                && slot.cancel == CancelState::Wanted
                && self.now_ms >= slot.cancel_not_before_ms
            {
                if let Some(cloid) = slot.cloid {
                    chosen[count] = index as u32;
                    cancels[count] = CancelCloidWire {
                        asset: self.market(slot.market_index).config.market.asset(),
                        cloid,
                    };
                    count += 1;
                }
            }
        }
        assert!(count <= BATCH_SIZE_MAX);
        assert!(chosen[..count].iter().all(|&at| self.slots[at as usize].cloid.is_some()));
        count
    }

    fn transmit_status(&mut self, request_index: usize, target: &mut [u8]) -> Option<Transmit> {
        assert!(!self.requests[request_index].active);
        if self.status_budget == 0 {
            return None;
        }
        let index = self.slots.iter().position(|slot| self.status_due(slot))?;
        if !self.bucket.take(self.now_ms, WEIGHT_STATUS, self.config.weight_cancel_reserve) {
            return None;
        }
        self.status_budget -= 1;
        assert!(self.status_budget < STATUS_QUERIES_PER_TICK);
        let slot = self.slots[index];
        let order_target = match (slot.cloid, slot.oid) {
            (Some(cloid), _) => OrderRef::Cloid(cloid),
            (None, oid) => {
                assert!(oid != 0, "an order with neither id could not have been adopted");
                OrderRef::Oid(oid)
            }
        };
        let request = Info::OrderStatus { user: self.config.user, target: order_target };
        let size = write_body(&request, target);
        let slot_ids = [index as u32];
        let request_id = self.occupy_request(request_index, RequestKind::Status, &slot_ids);
        self.slots[index].poll_ms =
            self.now_ms.saturating_add(u64::from(self.config.status_poll_ms));
        Some(Transmit { request_id, endpoint: Endpoint::Info, size })
    }

    fn status_due(&self, slot: &Slot) -> bool {
        assert!(slot.state != State::Free || slot.poll_ms == u64::MAX);
        if self.now_ms < slot.poll_ms {
            return false;
        }
        match slot.state {
            // Before expiry plus margin, "unknown" would prove nothing, so it is not worth asking.
            State::Ambiguous => {
                self.now_ms >= slot.deadline_ms + u64::from(self.config.settle_margin_ms)
            }
            State::Live => true,
            State::Free | State::Queued | State::Inflight | State::Settling => false,
        }
    }

    fn transmit_snapshot(&mut self, request_index: usize, target: &mut [u8]) -> Option<Transmit> {
        assert!(!self.requests[request_index].active);

        assert!(target.len() >= REQUEST_SIZE_MAX);
        if self.now_ms < self.reads_not_before_ms {
            return None;
        }
        let user = self.config.user;
        let (kind, info) = if self.need_open_orders == Need::Wanted {
            (RequestKind::OpenOrders, Info::FrontendOpenOrders { user, dex: None })
        } else if self.need_positions == Need::Wanted && self.now_ms >= self.positions_not_before_ms
        {
            (RequestKind::Positions, Info::ClearinghouseState { user, dex: None })
        } else if self.need_fills == Need::Wanted {
            (RequestKind::Fills, Info::UserFills { user, aggregate_by_time: false })
        } else {
            return None;
        };
        // Recovery may spend what is there: the bucket starts empty, and it holds no orders to
        // cancel.
        let reserve = if self.is_running() { self.config.weight_cancel_reserve } else { 0 };
        if !self.bucket.take(self.now_ms, info.weight(), reserve) {
            return None;
        }
        match kind {
            RequestKind::OpenOrders => self.need_open_orders = Need::Inflight,
            RequestKind::Positions => self.need_positions = Need::Inflight,
            RequestKind::Fills => self.need_fills = Need::Inflight,
            RequestKind::Place | RequestKind::Cancel | RequestKind::Status => {
                panic!("the snapshot branch above only picks snapshot kinds");
            }
        }
        let size = write_body(&info, target);
        let request_id = self.occupy_request(request_index, kind, &[]);
        assert!(request_id > 0);
        Some(Transmit { request_id, endpoint: Endpoint::Info, size })
    }

    /// Records what became of a request. Replies for requests already given up on are ignored: the
    /// orders they named are being settled by the path that cannot be wrong.
    pub fn on_reply(&mut self, now_ms: u64, request_id: u64, reply: Reply<'_, '_>) {
        self.advance_time(now_ms);
        let Some(index) = self.requests.iter().position(|r| r.active && r.id == request_id) else {
            return;
        };
        let request = self.requests[index];
        self.requests[index] = REQUEST_FREE;
        assert!(request.active);
        assert!(request.id == request_id);
        match reply {
            Reply::Failed => self.request_failed(&request),
            Reply::Throttled => self.request_throttled(&request),
            Reply::Value(value) => self.request_replied(&request, value),
        }
    }

    fn request_failed(&mut self, request: &Request) {
        assert!(usize::from(request.slots_count) <= BATCH_SIZE_MAX);

        assert!(request.kind != RequestKind::Status || request.slots_count == 1);
        match request.kind {
            RequestKind::Place => {
                for position in 0..usize::from(request.slots_count) {
                    if let Some(index) = self.resolve(request.slots[position]) {
                        self.mark_ambiguous(index);
                    }
                }
            }
            RequestKind::Cancel => {
                for position in 0..usize::from(request.slots_count) {
                    if let Some(index) = self.resolve(request.slots[position]) {
                        self.cancel_again_later(index);
                    }
                }
            }
            RequestKind::Status => {
                // The slot's poll time already passed the retry forward.
            }
            RequestKind::OpenOrders | RequestKind::Positions | RequestKind::Fills => {
                self.read_again(request.kind);
            }
        }
    }

    /// A read failed, or answered something unreadable, so it is owed again after a pause. Recovery
    /// does not finish on a read it could not understand: not knowing is not the same as nothing.
    fn read_again(&mut self, kind: RequestKind) {
        match kind {
            RequestKind::OpenOrders => self.need_open_orders = Need::Wanted,
            RequestKind::Positions => self.need_positions = Need::Wanted,
            RequestKind::Fills => self.need_fills = Need::Wanted,
            RequestKind::Place | RequestKind::Cancel | RequestKind::Status => {
                panic!("only snapshot reads are read again");
            }
        }
        self.reads_not_before_ms = self.now_ms + u64::from(self.config.status_poll_ms);
        assert!(self.reads_not_before_ms > self.now_ms);
    }

    fn request_throttled(&mut self, request: &Request) {
        assert!(usize::from(request.slots_count) <= BATCH_SIZE_MAX);
        self.throttled_until_ms = self.now_ms + u64::from(self.config.status_poll_ms);
        if request.kind == RequestKind::Place {
            for position in 0..usize::from(request.slots_count) {
                if let Some(index) = self.resolve(request.slots[position]) {
                    if self.slots[index].state == State::Inflight {
                        self.finish_slot(index, Finish::Throttled);
                    }
                }
            }
        } else {
            // Nothing happened, so these are exactly as owed as they were.
            self.request_failed(request);
        }
    }

    fn mark_ambiguous(&mut self, index: usize) {
        assert!(self.slots[index].state != State::Free);
        if self.slots[index].state == State::Inflight {
            let slot = &mut self.slots[index];
            slot.state = State::Ambiguous;
            slot.poll_ms = 0;
            let cloid = slot.cloid;
            if let Some(cloid) = cloid {
                self.push_event(Event::Ambiguous { cloid });
            }
            assert!(self.slots[index].state == State::Ambiguous);
        }
    }

    fn cancel_again_later(&mut self, index: usize) {
        assert!(index < ORDERS);
        assert!(self.slots[index].state != State::Free);
        let poll_ms = u64::from(self.config.status_poll_ms);
        let slot = &mut self.slots[index];
        if slot.cancel == CancelState::Sent {
            // A cancel the exchange keeps refusing must not become a steady spend of the budget.
            let doublings = slot.cancel_retries.min(CANCEL_BACKOFF_DOUBLINGS_MAX);
            slot.cancel = CancelState::Wanted;
            slot.cancel_not_before_ms = self.now_ms + (poll_ms << doublings);
            slot.cancel_retries = slot.cancel_retries.saturating_add(1);
        }
        assert!(self.slots[index].cancel != CancelState::Sent);
    }

    fn request_replied(&mut self, request: &Request, value: Value<'_, '_>) {
        match request.kind {
            RequestKind::Place => self.place_replied(request, value),
            RequestKind::Cancel => self.cancel_replied(request, value),
            RequestKind::Status => {
                assert_eq!(request.slots_count, 1);
                if let Some(index) = self.resolve(request.slots[0]) {
                    self.status_replied(index, value);
                }
            }
            RequestKind::OpenOrders => self.open_orders_replied(value),
            RequestKind::Positions => self.positions_replied(request, value),
            RequestKind::Fills => self.fills_replied(value),
        }
    }

    fn place_replied(&mut self, request: &Request, value: Value<'_, '_>) {
        assert!(request.kind == RequestKind::Place);

        assert!(request.slots_count > 0);
        let slots = &request.slots[..usize::from(request.slots_count)];
        match ExchangeReply::parse(value) {
            Some(ExchangeReply::Err(_)) => {
                for &slot in slots {
                    if let Some(index) = self.resolve(slot) {
                        if self.slots[index].state == State::Inflight {
                            self.finish_slot(index, Finish::Rejected);
                        }
                    }
                }
            }
            Some(ExchangeReply::Ok { statuses: Some(statuses), .. })
                if statuses.kind() == Kind::Array && statuses.count() as usize == slots.len() =>
            {
                for (&slot, status) in slots.iter().zip(statuses.items()) {
                    if let Some(index) = self.resolve(slot) {
                        self.place_status(index, PlaceStatus::parse(status));
                    }
                }
            }
            // The reply cannot be lined up with the batch, so it decides nothing.
            Some(ExchangeReply::Ok { .. }) | None => self.request_failed(request),
        }
    }

    fn place_status(&mut self, index: usize, status: PlaceStatus<'_>) {
        assert!(self.slots[index].state != State::Free);

        assert!(self.slots[index].state != State::Queued);
        let state = self.slots[index].state;
        if !matches!(state, State::Inflight | State::Ambiguous) {
            // The feed got here first, and it is at least as well informed.
            return;
        }
        match status {
            PlaceStatus::Resting { oid, .. } => self.mark_live(index, oid),
            PlaceStatus::Filled { total_sz, oid, .. } => {
                self.slots[index].oid = oid;
                let slot = self.slots[index];
                if slot.tif == Tif::Ioc {
                    self.mark_settling(index, total_sz);
                } else if total_sz >= slot.sz {
                    self.mark_settling(index, slot.sz);
                } else {
                    // A partial on a resting order: the remainder lives on.
                    self.mark_live(index, oid);
                }
            }
            PlaceStatus::Error(_) => self.finish_slot(index, Finish::Rejected),
            PlaceStatus::Unrecognized => self.mark_ambiguous(index),
        }
        assert!(self.slots[index].state != State::Inflight, "a verdict was reached");
    }

    fn mark_live(&mut self, index: usize, oid: u64) {
        assert_ne!(self.slots[index].state, State::Free);
        assert_ne!(self.slots[index].state, State::Settling);
        let slot = &mut self.slots[index];
        let was_known = slot.state == State::Live;
        slot.state = State::Live;
        slot.oid = oid;
        // A live order is watched by the feed and the reconcile, and queried only under suspicion.
        slot.poll_ms = u64::MAX;
        let cloid = slot.cloid;
        if !was_known {
            if let Some(cloid) = cloid {
                self.push_event(Event::Resting { cloid, oid });
            }
        }
    }

    fn mark_settling(&mut self, index: usize, sz_final: Sz) {
        assert!(index < ORDERS);
        let slot = &mut self.slots[index];
        assert_ne!(slot.state, State::Free);
        slot.finish = if sz_final >= slot.sz { Finish::Filled } else { Finish::Dead };
        slot.sz_final = sz_final;
        slot.state = State::Settling;
        slot.deadline_ms = self.now_ms + u64::from(self.config.fills_wait_ms);
        slot.settled_ms = self.now_ms;
        slot.fills_retries = 0;
        self.finish_if_settled(index);
    }

    fn finish_if_settled(&mut self, index: usize) {
        assert!(index < ORDERS);
        let slot = self.slots[index];
        if slot.state == State::Settling && slot.sz_filled >= slot.sz_final {
            self.finish_slot(index, slot.finish);
            assert!(self.slots[index].state == State::Free);
        }
    }

    fn cancel_replied(&mut self, request: &Request, value: Value<'_, '_>) {
        assert!(request.kind == RequestKind::Cancel);

        assert!(request.slots_count > 0);
        let slots = &request.slots[..usize::from(request.slots_count)];
        match ExchangeReply::parse(value) {
            Some(ExchangeReply::Ok { statuses: Some(statuses), .. })
                if statuses.kind() == Kind::Array && statuses.count() as usize == slots.len() =>
            {
                for (&slot, status) in slots.iter().zip(statuses.items()) {
                    if let Some(index) = self.resolve(slot) {
                        self.cancel_status(index, CancelStatus::parse(status));
                    }
                }
            }
            Some(ExchangeReply::Err(_) | ExchangeReply::Ok { .. }) | None => {
                self.request_failed(request);
            }
        }
    }

    fn cancel_status(&mut self, index: usize, status: CancelStatus<'_>) {
        assert!(self.slots[index].state != State::Free);

        assert!(self.slots[index].state != State::Queued);
        match status {
            CancelStatus::Success => {
                // The feed will say how much filled first. If it does not, the status query will.
                let slot = &mut self.slots[index];
                if slot.state == State::Live {
                    slot.poll_ms = self.now_ms + u64::from(self.config.status_poll_ms);
                }
            }
            // "Never placed, already canceled, or filled": it does not say which, so ask. The
            // cancel stays wanted, because "never placed" may only mean "not yet".
            CancelStatus::Error(_) | CancelStatus::Unrecognized => {
                self.cancel_again_later(index);
                let slot = &mut self.slots[index];
                if slot.state == State::Live {
                    slot.poll_ms = self.now_ms;
                }
            }
        }
    }

    fn status_replied(&mut self, index: usize, value: Value<'_, '_>) {
        assert!(self.slots[index].state != State::Free);

        assert!(self.slots[index].state != State::Queued);
        match OrderStatusReply::parse(value) {
            Some(OrderStatusReply::Found(update)) => self.apply_update(index, &update),
            Some(OrderStatusReply::Unknown) => {
                let slot = self.slots[index];
                let settled_ms = slot.deadline_ms + u64::from(self.config.settle_margin_ms);
                // The whole design rests on this comparison. Past the deadline an unknown id cannot
                // land any more; before it, the same answer means nothing at all. `status_due` is
                // what keeps the question from being asked early, and this is the pair to it.
                if slot.state == State::Ambiguous {
                    assert!(self.now_ms >= settled_ms, "an ambiguous order was queried early");
                    self.finish_slot(index, Finish::NeverLanded);
                }
            }
            None => {}
        }
    }

    /// Applies what the exchange says about an order. Idempotent, and the lifecycle only moves
    /// forward: a stale `open` arriving after `canceled` changes nothing.
    fn apply_update(&mut self, index: usize, update: &OrderUpdate) {
        assert!(index < ORDERS);
        assert_ne!(self.slots[index].state, State::Free);
        let order = &update.order;
        match update.class {
            StatusClass::Live => {
                if self.slots[index].state != State::Settling {
                    self.mark_live(index, order.oid);
                    if self.slots[index].cancel == CancelState::Sent {
                        // Still open after a cancel went out: try again, and keep looking.
                        self.cancel_again_later(index);
                        self.slots[index].poll_ms =
                            self.now_ms + u64::from(self.config.status_poll_ms);
                    }
                }
            }
            StatusClass::Filled => {
                self.slots[index].oid = order.oid;
                if self.slots[index].state != State::Settling {
                    // Mainnet's history has `filled` with a remainder, such as an IOC cut short, so
                    // the word alone does not mean all of it.
                    self.mark_settling(index, order.sz_orig.saturating_sub(order.sz));
                }
            }
            StatusClass::Dead => {
                self.slots[index].oid = order.oid;
                if self.slots[index].state != State::Settling {
                    self.mark_settling(index, order.sz_orig.saturating_sub(order.sz));
                }
            }
            StatusClass::Unknown => {
                if let Some(cloid) = self.slots[index].cloid {
                    self.push_event(Event::StatusUnread { cloid });
                }
                assert!(self.slots[index].state != State::Free, "an unread word frees nothing");
            }
        }
    }

    /// Reads a WebSocket message from the account feeds. Returns whether it was one; anything else,
    /// such as market data, is the caller's.
    pub fn on_message(&mut self, now_ms: u64, message: &Message<'_, '_>) -> bool {
        self.advance_time(now_ms);
        assert!(self.now_ms >= now_ms);
        let Some(data) = message.data else {
            return false;
        };
        match message.channel {
            Channel::OrderUpdates => {
                for item in data.items() {
                    if let Some(update) = OrderUpdate::parse(item) {
                        self.feed_order_update(&update);
                    }
                }
                true
            }
            Channel::UserFills | Channel::UserEvents => {
                if let Some(fills) = data.get("fills") {
                    self.apply_fills(fills, FillOrder::OldestFirst);
                }
                true
            }
            _ => false,
        }
    }

    fn feed_order_update(&mut self, update: &OrderUpdate) {
        let index = match update.order.cloid {
            Some(cloid) => self.slot_of_cloid(cloid),
            None => self.slot_of_oid(update.order.oid),
        };
        if let Some(index) = index {
            assert!(self.slots[index].state != State::Free);
            self.apply_update(index, update);
        }
    }

    /// Applies a list of fills oldest first, since position tracking depends on the order. The
    /// caller says how the list is sorted, because it knows: `/info` sends newest first and the
    /// feed oldest first, per captured traffic. Inferring it from timestamps fails on a list from a
    /// single block, where they are all equal and the fills would be counted twice.
    fn apply_fills(&mut self, fills: Value<'_, '_>, sorted: FillOrder) {
        let mut indices = [0u32; FILLS_LIST_MAX];
        let count = fills.item_indices(&mut indices);
        assert!(count <= FILLS_LIST_MAX);
        for position in 0..count {
            let at = match sorted {
                FillOrder::NewestFirst => count - 1 - position,
                FillOrder::OldestFirst => position,
            };
            assert!(at < count);
            match Fill::parse(fills.at_index(indices[at])) {
                Some(fill) => self.apply_fill(&fill),
                None => self.unreadable_count += 1,
            }
        }
    }

    /// Counts a fill once. Position moves first and on its own, because a fill is true whether or
    /// not this engine has a slot for it, and old fills still anchor it. Fills from before this
    /// process started are history, not news, so they stop there.
    fn apply_fill(&mut self, fill: &Fill) {
        if fill.time_ms > self.now_ms.saturating_add(FILL_AHEAD_MS_MAX) {
            self.unreadable_count += 1;
            return;
        }
        let key = FillKey { time_ms: fill.time_ms, tid: fill.tid, oid: fill.oid };
        if self.fills_seen.contains(&key) {
            return;
        }
        self.fills_seen[self.fills_seen_next] = key;
        self.fills_seen_next = (self.fills_seen_next + 1) % FILLS_SEEN_MAX;
        assert!(self.fills_seen_next < FILLS_SEEN_MAX);

        let Some(market_index) = self.market_index_of(&fill.coin) else {
            return;
        };
        self.move_position(market_index, fill);
        if fill.time_ms <= self.fills_floor_ms {
            return;
        }
        self.push_event(Event::Filled {
            cloid: fill.cloid,
            market_index,
            side: fill.side,
            px: fill.px,
            sz: fill.sz,
            time_ms: fill.time_ms,
        });
        self.credit_fill(fill);
    }

    /// A fill from a newer block restates the position outright, from the exchange's own
    /// `startPosition`, so a missed fill or a stale read heals on the next fill. Within one block
    /// fills add up. An older fill is already inside the position a newer one restated.
    fn move_position(&mut self, market_index: u16, fill: &Fill) {
        assert!(market_index < self.markets_count);
        let market = self.market_mut(market_index);
        if fill.time_ms < market.fill_time_last_ms {
            return;
        }
        let base = match fill.start_position {
            Some(start) if fill.time_ms > market.fill_time_last_ms => start,
            _ => market.position,
        };
        let moved = if fill.side.is_buy() {
            base.checked_add_sz(fill.sz)
        } else {
            base.checked_sub_sz(fill.sz)
        };
        // An exchange-reported size past i128 cannot be a real position, so it moves nothing.
        if let Some(position) = moved {
            market.position = position;
            market.fill_time_last_ms = fill.time_ms;
        }
    }

    fn credit_fill(&mut self, fill: &Fill) {
        let index = match fill.cloid {
            Some(cloid) => self.slot_of_cloid(cloid),
            None => self.slot_of_oid(fill.oid),
        };
        let Some(index) = index else {
            return;
        };
        assert!(self.slots[index].state != State::Free);
        if fill.time_ms <= self.slots[index].credit_floor_ms {
            return;
        }
        if let Some(total) = self.slots[index].sz_filled.checked_add(fill.sz) {
            self.slots[index].sz_filled = total;
        }
        // A fill is proof it landed, whatever the request's fate looked like.
        if matches!(self.slots[index].state, State::Inflight | State::Ambiguous) {
            self.mark_live(index, fill.oid);
        }
        if self.slots[index].state == State::Live
            && self.slots[index].sz_filled >= self.slots[index].sz
        {
            let total = self.slots[index].sz;
            self.mark_settling(index, total);
        }
        self.finish_if_settled(index);
        let free = self.slots[index].state == State::Free;
        assert!(free || !matches!(self.slots[index].state, State::Inflight | State::Ambiguous));
    }

    fn fills_replied(&mut self, value: Value<'_, '_>) {
        assert!(self.is_running() || matches!(self.phase, Phase::Snapshot));
        assert_eq!(self.need_fills, Need::Inflight);
        self.need_fills = Need::Idle;
        if value.kind() != Kind::Array {
            self.unreadable_count += 1;
            self.read_again(RequestKind::Fills);
            return;
        }
        // The docs add weight per 20 items, and only now is the count known.
        self.bucket.charge(self.now_ms, value.count() / 20);
        self.apply_fills(value, FillOrder::NewestFirst);
        assert!(self.need_fills != Need::Inflight, "applying fills sends nothing");
    }

    fn open_orders_replied(&mut self, value: Value<'_, '_>) {
        assert!(!matches!(self.phase, Phase::Quarantine { .. }));
        assert_eq!(self.need_open_orders, Need::Inflight);
        self.need_open_orders = Need::Idle;
        assert!(self.need_positions != Need::Inflight || self.is_running());
        if !self.open_orders_readable(value) {
            self.unreadable_count += 1;
            self.read_again(RequestKind::OpenOrders);
            return;
        }
        let mut seen = [false; ORDERS];
        for item in value.items() {
            if let Some(order) = OrderView::parse(item) {
                self.reconcile_open_order(&order, &mut seen);
            }
        }
        // A live order the exchange does not list has probably finished unseen. Stale reads make
        // this only a suspicion, so it earns a question and not a conclusion.
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.state == State::Live && !seen[index] && slot.poll_ms == u64::MAX {
                slot.poll_ms = self.now_ms;
            }
        }
        if matches!(self.phase, Phase::Snapshot) {
            // Two reads can see two different moments, and an order finishing between them would be
            // in neither. Reading positions a full margin later means an order missing from the
            // list above, finished before that read's moment, is certainly in this one's position.
            self.need_positions = Need::Wanted;
            self.positions_not_before_ms = self.now_ms + u64::from(self.config.settle_margin_ms);
        }
        self.finish_snapshot_if_loaded();
    }

    /// Whether the whole list can be taken at its word. One unreadable entry on a traded market
    /// means an order the bound would miss, so the read is refused and not skimmed. Entries on
    /// other markets cannot affect the bound.
    fn open_orders_readable(&self, value: Value<'_, '_>) -> bool {
        assert!(self.markets_count > 0);
        if value.kind() != Kind::Array {
            return false;
        }
        value.items().all(|item| {
            let Some(coin) = item.get("coin").and_then(Value::text).and_then(Coin::parse) else {
                return false;
            };
            self.market_index_of(&coin).is_none() || OrderView::parse(item).is_some()
        })
    }

    fn reconcile_open_order(&mut self, order: &OrderView, seen: &mut [bool; ORDERS]) {
        assert!(self.markets_count > 0);
        let existing = match order.cloid {
            Some(cloid) => self.slot_of_cloid(cloid),
            None => self.slot_of_oid(order.oid),
        };
        if let Some(index) = existing {
            seen[index] = true;
            if matches!(self.slots[index].state, State::Inflight | State::Ambiguous) {
                self.mark_live(index, order.oid);
            }
        } else if let Some(market_index) = self.market_index_of(&order.coin) {
            self.adopt(order, market_index, seen);
        }
    }

    /// Takes in an open order this engine did not place, or placed in a previous life. Its size
    /// counts against the bound either way, which is why this is not optional.
    fn adopt(&mut self, order: &OrderView, market_index: u16, seen: &mut [bool; ORDERS]) {
        assert!(market_index < self.markets_count);
        assert!(self.market_index_of(&order.coin) == Some(market_index));
        // A TP/SL on a whole position has no size of its own: mainnet sends `origSz` of 0.0 for it.
        // It only closes what is already counted, so there is nothing to add to the bound. An order
        // with no id of either kind could not be tracked, and the exchange does not send one.
        if order.sz_orig == Sz::ZERO || (order.cloid.is_none() && order.oid == 0) {
            return;
        }
        let Some(index) = self.slots.iter().position(|slot| slot.state == State::Free) else {
            self.halt(HaltReason::SlotsExhausted);
            return;
        };
        self.slots[index] = Slot {
            state: State::Live,
            cloid: order.cloid,
            oid: order.oid,
            market_index,
            side: order.side,
            reduce_only: order.reduce_only,
            is_trigger: order.is_trigger,
            px: order.limit_px,
            sz: order.sz_orig,
            sz_filled: order.sz_orig.saturating_sub(order.sz),
            credit_floor_ms: self.now_ms + u64::from(self.config.settle_margin_ms),
            generation: self.slots[index].generation,
            ..SLOT_FREE
        };
        seen[index] = true;
        assert!(self.slots[index].state == State::Live);
        self.push_event(Event::Adopted { cloid: order.cloid, oid: order.oid });
    }

    fn positions_replied(&mut self, request: &Request, value: Value<'_, '_>) {
        assert!(!matches!(self.phase, Phase::Quarantine { .. }));
        assert!(self.need_positions == Need::Inflight);
        self.need_positions = Need::Idle;
        let time_ms = value.get("time").and_then(Value::u64).filter(|time_ms| *time_ms > 0);
        let reported = self.read_positions(value);
        let (Some(time_ms), Some(reported)) = (time_ms, reported) else {
            self.unreadable_count += 1;
            self.read_again(RequestKind::Positions);
            return;
        };
        self.check_clock(request, time_ms);
        if matches!(self.phase, Phase::Snapshot) {
            self.load_positions(&reported, time_ms);
        } else if self.is_quiet(time_ms) {
            self.check_positions(&reported);
        } else {
            self.close_overdue(&reported, time_ms);
        }
        self.finish_snapshot_if_loaded();
    }

    /// The server's clock must fall within the time this request was out, give or take the margin.
    /// Past that, the deadline this design rests on cannot be believed, so no new risk is taken.
    fn check_clock(&mut self, request: &Request, time_ms: u64) {
        let margin_ms = u64::from(self.config.settle_margin_ms);
        assert!(margin_ms > 0);
        assert!(request.kind == RequestKind::Positions);
        let sent_ms = request.deadline_ms.saturating_sub(u64::from(self.config.request_timeout_ms));
        assert!(sent_ms <= self.now_ms);
        let behind = time_ms.saturating_add(margin_ms) < sent_ms;
        let ahead = time_ms > self.now_ms.saturating_add(margin_ms);
        if behind || ahead {
            self.halt(HaltReason::ClockSkew);
        }
    }

    /// Every tracked market's position, or `None` unless the whole list can be read. An entry that
    /// fails to parse must not read as flat: that would drop a real position from the bound.
    fn read_positions(&self, value: Value<'_, '_>) -> Option<[Szi; MARKETS]> {
        assert!(self.markets_count > 0);
        let list = value.get("assetPositions")?;
        if list.kind() != Kind::Array {
            return None;
        }
        let mut positions = [Szi::ZERO; MARKETS];
        for item in list.items() {
            let position = crate::feed::Position::parse(item)?;
            if let Some(market_index) = self.market_index_of(&position.coin) {
                positions[usize::from(market_index)] = position.szi;
            }
        }
        assert!(self.markets_count as usize <= MARKETS);
        Some(positions)
    }

    /// Whether a position read could be taken at its word. Any order in motion can legitimately put
    /// it out of step with the fills seen. So can a stale read, whose `time` says when it was
    /// answered and not how old its state is: only a read later than the newest fill by the whole
    /// staleness margin is certain to include that fill.
    fn is_quiet(&self, time_ms: u64) -> bool {
        assert!(self.markets_count > 0);
        let moving = self.slots.iter().any(|slot| {
            matches!(
                slot.state,
                State::Queued | State::Inflight | State::Ambiguous | State::Settling
            )
        });
        let newest_fill_ms = (0..self.markets_count)
            .map(|index| self.market(index).fill_time_last_ms)
            .max()
            .unwrap_or(0);
        !moving && time_ms > newest_fill_ms.saturating_add(u64::from(self.config.settle_margin_ms))
    }

    /// Ends the wait for fills that never came. A read this much newer than both the order's end
    /// and the market's newest fill must include those fills, so it can replace the position, and
    /// the unseen size is no longer unseen.
    fn close_overdue(&mut self, reported: &[Szi; MARKETS], time_ms: u64) {
        assert!(self.config.settle_margin_ms > 0);
        let margin_ms = u64::from(self.config.settle_margin_ms);
        for market_index in 0..self.markets_count {
            let newest_fill_ms = self.market(market_index).fill_time_last_ms;
            let past_fills = time_ms > newest_fill_ms.saturating_add(margin_ms);
            if self.tracks_position(market_index) && past_fills {
                self.close_overdue_market(
                    reported[usize::from(market_index)],
                    time_ms,
                    market_index,
                );
            }
        }
    }

    fn close_overdue_market(&mut self, reported: Szi, time_ms: u64, market_index: u16) {
        assert!(market_index < self.markets_count);
        assert!(self.tracks_position(market_index));
        let margin_ms = u64::from(self.config.settle_margin_ms);
        let mut closed_any = false;
        for index in 0..ORDERS {
            let slot = self.slots[index];
            if slot.state == State::Settling
                && slot.market_index == market_index
                && slot.fills_retries > 0
                && time_ms >= slot.settled_ms + margin_ms
            {
                self.close_missing(index);
                closed_any = true;
                assert!(self.slots[index].state == State::Free);
            }
        }
        if closed_any {
            self.restate_position(market_index, reported);
        }
    }

    fn restate_position(&mut self, market_index: u16, reported: Szi) {
        let held = self.market(market_index).position;
        if reported != held {
            self.market_mut(market_index).position = reported;
            self.push_event(Event::PositionCorrected { market_index, from: held, to: reported });
        }
        assert!(self.market(market_index).position == reported);
    }

    /// A fill seen during quarantine restated the position exactly, and a stale read must not undo
    /// that, so a market whose newest fill the read may have missed keeps the fill's word for it.
    fn load_positions(&mut self, reported: &[Szi; MARKETS], time_ms: u64) {
        assert!(matches!(self.phase, Phase::Snapshot));
        let margin_ms = u64::from(self.config.settle_margin_ms);
        for market_index in 0..self.markets_count {
            let newest_fill_ms = self.market(market_index).fill_time_last_ms;
            let past_fills = time_ms > newest_fill_ms.saturating_add(margin_ms);
            if self.tracks_position(market_index) && past_fills {
                self.market_mut(market_index).position = reported[usize::from(market_index)];
            }
        }
    }

    fn check_positions(&mut self, reported: &[Szi; MARKETS]) {
        assert!(self.is_running());
        for market_index in 0..self.markets_count {
            if self.tracks_position(market_index) {
                self.restate_position(market_index, reported[usize::from(market_index)]);
            }
        }
    }

    /// `clearinghouseState` covers the default dex's perps. Spot and builder dex positions are
    /// tracked from fills alone, relative to where the engine started.
    fn tracks_position(&self, market_index: u16) -> bool {
        assert!(market_index < self.markets_count);
        let asset: Asset = self.market(market_index).config.market.asset();
        matches!(asset.kind(), AssetKind::Perp)
    }

    fn finish_snapshot_if_loaded(&mut self) {
        assert!(!matches!(self.phase, Phase::Quarantine { .. }));
        if matches!(self.phase, Phase::Snapshot)
            && self.need_open_orders == Need::Idle
            && self.need_positions == Need::Idle
        {
            self.phase = Phase::Running;
            self.reconcile_next_ms = self.now_ms + u64::from(self.config.reconcile_ms);
            self.push_event(Event::Recovered);
        }
    }

    /// The slot a request meant, if the order it meant is still the one there.
    fn resolve(&self, slot: SlotRef) -> Option<usize> {
        assert!((slot.index as usize) < ORDERS);
        let held = &self.slots[slot.index as usize];
        if held.state != State::Free && held.generation == slot.generation {
            Some(slot.index as usize)
        } else {
            None
        }
    }

    fn slot_of_cloid(&self, cloid: Cloid) -> Option<usize> {
        let found = self
            .slots
            .iter()
            .position(|slot| slot.state != State::Free && slot.cloid == Some(cloid));
        assert!(found.is_none_or(|at| self.slots[at].cloid == Some(cloid)));
        found
    }

    fn slot_of_oid(&self, oid: u64) -> Option<usize> {
        // Zero means "not yet named" in a slot, so it must never match one.
        if oid == 0 {
            return None;
        }
        let found = self.slots.iter().position(|slot| slot.state != State::Free && slot.oid == oid);
        assert!(found.is_none_or(|at| self.slots[at].oid == oid));
        found
    }

    fn finish_slot(&mut self, index: usize, finish: Finish) {
        let slot = self.slots[index];
        assert_ne!(slot.state, State::Free);
        self.slots[index] = Slot { generation: slot.generation.wrapping_add(1), ..SLOT_FREE };
        if slot.cloid.is_some() {
            self.cloids_seen[self.cloids_seen_next] = slot.cloid;
            self.cloids_seen_next = (self.cloids_seen_next + 1) % CLOIDS_SEEN_MAX;
            assert!(self.cloids_seen_next < CLOIDS_SEEN_MAX);
        }
        assert!(self.slots[index].generation != slot.generation, "stale replies must not match");
        self.push_event(Event::Finished { cloid: slot.cloid, sz_filled: slot.sz_filled, finish });
    }

    fn halt(&mut self, reason: HaltReason) {
        if self.halted.is_none() {
            self.halted = Some(reason);
            self.push_event(Event::Halted { reason });
        }
        assert!(self.halted.is_some());
    }

    fn push_event(&mut self, event: Event) {
        if self.events_count == EVENTS_MAX {
            // The oldest facts are kept, since the newest can be re-derived from the exchange. No
            // new risk is taken while the caller is not listening.
            self.halted = Some(HaltReason::EventsFull);
            return;
        }
        let tail = (self.event_head + self.events_count) % EVENTS_MAX;
        assert!(self.events[tail].is_none());
        self.events[tail] = Some(event);
        self.events_count += 1;
        assert!(self.events_count <= EVENTS_MAX);
    }

    /// The oldest fact not yet taken.
    pub fn poll_event(&mut self) -> Option<Event> {
        if self.events_count == 0 {
            return None;
        }
        let event = self.events[self.event_head].take();
        assert!(event.is_some());
        self.event_head = (self.event_head + 1) % EVENTS_MAX;
        self.events_count -= 1;
        assert!(self.event_head < EVENTS_MAX);
        event
    }

    /// How many orders are alive, in any state.
    #[must_use]
    pub fn orders_count(&self) -> u32 {
        let count = self.slots.iter().filter(|slot| slot.state != State::Free).count();
        assert!(count <= ORDERS);
        count as u32
    }

    /// How many orders have an unsettled outcome.
    #[must_use]
    pub fn ambiguous_count(&self) -> u32 {
        let count = self.slots.iter().filter(|slot| slot.state == State::Ambiguous).count();
        assert!(count <= ORDERS);
        count as u32
    }

    /// Whether nothing is in motion and nothing is owed: the state the simulator converges to.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        let unresolved =
            self.slots.iter().any(|slot| !matches!(slot.state, State::Free | State::Live));
        let cancels = self.slots.iter().any(|slot| slot.cancel != CancelState::None);
        let requests = self.requests.iter().any(|request| {
            request.active && matches!(request.kind, RequestKind::Place | RequestKind::Cancel)
        });
        let settled = self.is_running() && !unresolved && !cancels && !requests;
        assert!(!settled || self.ambiguous_count() == 0);
        settled
    }

    /// What this engine believes about an order, for inspection.
    #[must_use]
    pub fn order_view(&self, cloid: Cloid) -> Option<OrderInspect> {
        assert!(ORDERS > 0);
        let index = self.slot_of_cloid(cloid)?;
        let slot = &self.slots[index];
        assert!(slot.state != State::Free);
        Some(OrderInspect {
            oid: slot.oid,
            sz: slot.sz,
            sz_filled: slot.sz_filled,
            is_live: slot.state == State::Live,
            is_ambiguous: slot.state == State::Ambiguous,
            cancel_pending: slot.cancel != CancelState::None,
        })
    }

    /// Checks the promises the code above relies on. Assertions compile into release builds, and
    /// this runs once per tick: the control plane paying for the data plane's confidence.
    fn assert_invariants(&self) {
        assert!(self.events_count <= EVENTS_MAX);
        assert!(self.fills_seen_next < FILLS_SEEN_MAX);
        assert!(self.cloids_seen_next < CLOIDS_SEEN_MAX);
        // Client ids are unique among live orders, which is what makes them an identity.
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.cloid.is_some() {
                let twin = self.slots[index + 1..].iter().any(|other| other.cloid == slot.cloid);
                assert!(!twin, "two live orders share a client id");
            }
        }
        for slot in &self.slots {
            if slot.state == State::Free {
                assert!(slot.cloid.is_none());
                assert_eq!(slot.cancel, CancelState::None);
                assert_eq!(slot.sz, Sz::ZERO);
            } else {
                assert!(slot.market_index < self.markets_count);
                assert!(slot.sz > Sz::ZERO, "submit and adopt both refuse a size of zero");
                assert!(slot.cloid.is_some() || slot.oid != 0);
            }
            if slot.state == State::Inflight {
                let carried = self.requests.iter().any(|request| {
                    request.active
                        && request.kind == RequestKind::Place
                        && request.id == slot.request_id
                });
                assert!(carried, "an in-flight order has a request carrying it");
            }
        }
    }
}

const PLACEHOLDER_ORDER: OrderWire = OrderWire {
    asset: Asset::from_raw(0),
    side: Side::Buy,
    px: Px::ZERO,
    sz: Sz::ZERO,
    reduce_only: false,
    kind: OrderKind::Limit(Tif::Gtc),
    cloid: None,
};

const PLACEHOLDER_CANCEL: CancelCloidWire =
    CancelCloidWire { asset: Asset::from_raw(0), cloid: Cloid([0; 16]) };

const fn lane_of(tif: Tif) -> Lane {
    match tif {
        Tif::Alo => Lane::Alo,
        Tif::Ioc | Tif::Gtc => Lane::Taker,
    }
}

const fn want(need: Need) -> Need {
    match need {
        // One on the wire already will answer; asking twice would only spend weight.
        Need::Inflight => Need::Inflight,
        Need::Idle | Need::Wanted => Need::Wanted,
    }
}

fn write_body<W: Walk>(value: &W, target: &mut [u8]) -> usize {
    assert!(target.len() >= REQUEST_SIZE_MAX);
    let mut sink = JsonSink::new(target);
    value.walk(&mut sink);
    let Ok(size) = sink.finish() else {
        panic!("REQUEST_SIZE_MAX is computed to hold the largest request");
    };
    assert!(size > 0);
    size
}

/// What the engine believes about one order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderInspect {
    pub oid: u64,
    pub sz: Sz,
    pub sz_filled: Sz,
    pub is_live: bool,
    pub is_ambiguous: bool,
    pub cancel_pending: bool,
}
