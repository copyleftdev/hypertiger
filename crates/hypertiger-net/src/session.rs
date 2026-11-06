//! A trading session: one WebSocket carrying the engine's requests and the account's feeds.

use hypertiger::engine::{
    EVENTS_MAX, Endpoint, Engine, Event, REQUEST_SIZE_MAX, REQUESTS_MAX, Reply,
};
use hypertiger::feed::{Channel, Message, PostKind, PostReply};
use hypertiger::info::{PingMessage, SubscribeMessage, Subscription};
use hypertiger::json::{Token, parse};
use hypertiger::limits::{WS_CONNECTIONS_NEW_PER_MINUTE, WS_PING_INTERVAL_MS};
use hypertiger::signing::Signer;
use hypertiger::sink::{JsonSink, Walk};
use hypertiger::types::{Address, Network};
use hypertiger::ws::CLIENT_HEADER_SIZE_MAX;

use crate::conn::{CONNECT_TIMEOUT_MS, Connection, NetError, Stream, WsClient, WsEvent};
use crate::{HOST_MAINNET, HOST_TESTNET, Host, now_ms};

/// The largest message expected. A full `userFills` snapshot is about 2,000 fills at 400 bytes.
pub const MESSAGE_SIZE_MAX: usize = 2 * 1024 * 1024;
/// Tape tokens for the largest message: about 34 per fill, and a margin.
pub const TOKENS_MAX: usize = 128 * 1024;
/// The wrapper a post adds around a request body:
/// `{"method":"post","id":N,"request":{"type":"action","payload":..}}`.
const POST_WRAPPER_SIZE_MAX: usize = 128;
/// How many messages one `step` handles, so that a firehose cannot starve the transmit side.
pub const MESSAGES_PER_STEP_MAX: u32 = 64;
/// The wait after a failed connection attempt, which doubles to a ceiling. The floor keeps to the
/// exchange's 30 new connections a minute.
pub const RECONNECT_BACKOFF_MS_MIN: u64 = 2_500;
pub const RECONNECT_BACKOFF_MS_MAX: u64 = 30_000;
/// Silence this long means the connection is dead, whatever TCP thinks. A ping goes out every 25 s
/// and is answered, and a half-open socket would otherwise take about 15 minutes to report, during
/// which cancels would go nowhere.
pub const RECEIVE_SILENCE_MS_MAX: u64 = 2 * WS_PING_INTERVAL_MS as u64;
/// The most the wall clock may part from the monotonic clock. A larger gap means a step, and the
/// deadlines this design rests on cannot be believed across one.
pub const CLOCK_STEP_MS_MAX: u64 = 250;

const _: () = assert!(TOKENS_MAX >= 34 * 2000 + 4096);
const _: () = assert!(MESSAGE_SIZE_MAX >= 400 * 2000);
const _: () = assert!(60_000 / RECONNECT_BACKOFF_MS_MIN <= WS_CONNECTIONS_NEW_PER_MINUTE as u64);

/// What opens a stream to a host. The real one dials TLS, and a test's hands back a stream it
/// controls.
pub trait Connector {
    type Stream: Stream;
    fn connect(&mut self, host: Host) -> Result<Self::Stream, NetError>;
}

/// Dials the exchange over TLS.
#[derive(Clone, Copy, Debug, Default)]
pub struct TlsConnector;

impl Connector for TlsConnector {
    type Stream = Connection;

    fn connect(&mut self, host: Host) -> Result<Connection, NetError> {
        Connection::connect(host)
    }
}

/// The two clocks a session needs, passed in so that a test can step one and not the other.
pub trait Clock {
    /// Milliseconds since the Unix epoch, which can step.
    fn wall_ms(&self) -> u64;
    /// Milliseconds from an arbitrary start, which cannot.
    fn monotonic_ms(&self) -> u64;
}

/// The operating system's clocks.
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
    started: std::time::Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> SystemClock {
        SystemClock { started: std::time::Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        now_ms()
    }

    fn monotonic_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// How a session is set up. The network and account come from the engine's config, and these must
/// match them, which `Session::new` checks.
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
    pub host: Host,
    pub user: Address,
}

/// One engine, one signer and one connection, stepped by the caller.
///
/// `on_market` receives every message that is neither a reply nor account state, so a strategy can
/// subscribe to market data on the same socket.
pub struct Session<
    const ORDERS: usize,
    const MARKETS: usize,
    A: Signer,
    C: Connector = TlsConnector,
    K: Clock = SystemClock,
> {
    config: SessionConfig,
    engine: Box<Engine<ORDERS, MARKETS>>,
    signer: A,
    connector: C,
    client: Option<WsClient<C::Stream>>,
    tokens: Vec<Token>,
    outgoing: Vec<u8>,
    request: Vec<u8>,
    inflight: [u64; REQUESTS_MAX],
    ping_next_ms: u64,
    connect_next_ms: u64,
    reconnect_backoff_ms: u64,
    received_last_ms: u64,
    clock: AnchoredClock<K>,
    pub connects: u64,
    pub messages: u64,
}

impl<const ORDERS: usize, const MARKETS: usize, A: Signer, C: Connector, K: Clock> std::fmt::Debug
    for Session<ORDERS, MARKETS, A, C, K>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Session(connects {}, messages {})", self.connects, self.messages)
    }
}

impl<const ORDERS: usize, const MARKETS: usize, A: Signer> Session<ORDERS, MARKETS, A> {
    /// A session over real TLS and the system's clocks.
    #[must_use]
    pub fn new(config: SessionConfig, engine: Box<Engine<ORDERS, MARKETS>>, signer: A) -> Self {
        Session::with_parts(config, engine, signer, TlsConnector, SystemClock::new())
    }
}

impl<const ORDERS: usize, const MARKETS: usize, A: Signer, C: Connector, K: Clock>
    Session<ORDERS, MARKETS, A, C, K>
{
    /// A session over a given connector and clock. A session's memory is allocated here, once,
    /// apart from one read buffer per connection and whatever TLS allocates.
    #[must_use]
    pub fn with_parts(
        config: SessionConfig,
        engine: Box<Engine<ORDERS, MARKETS>>,
        signer: A,
        connector: C,
        clock: K,
    ) -> Self {
        let network_host = match engine.config().network {
            Network::Mainnet => HOST_MAINNET,
            Network::Testnet => HOST_TESTNET,
        };
        assert!(config.host == network_host, "a signature for one network, sent to the other");
        assert!(config.user == engine.config().user, "the feeds and the reads must agree");
        Session {
            config,
            engine,
            signer,
            connector,
            client: None,
            tokens: vec![Token::ZERO; TOKENS_MAX],
            outgoing: vec![0; CLIENT_HEADER_SIZE_MAX + POST_WRAPPER_SIZE_MAX + REQUEST_SIZE_MAX],
            request: vec![0; REQUEST_SIZE_MAX],
            inflight: [0; REQUESTS_MAX],
            ping_next_ms: 0,
            connect_next_ms: 0,
            reconnect_backoff_ms: RECONNECT_BACKOFF_MS_MIN,
            received_last_ms: 0,
            clock: AnchoredClock::new(clock),
            connects: 0,
            messages: 0,
        }
    }

    #[must_use]
    pub fn engine(&self) -> &Engine<ORDERS, MARKETS> {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine<ORDERS, MARKETS> {
        &mut self.engine
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// Does one bounded pass of work: reads replies and feeds, ticks the engine, sends what is due.
    /// Connection failures are handled in here: every request in flight becomes ambiguous, and the
    /// next pass reconnects.
    pub fn step(&mut self, on_market: &mut dyn FnMut(&Message<'_, '_>)) {
        let now = self.now();
        if self.client.is_none() && now >= self.connect_next_ms {
            self.connect(now);
        }
        if let Err(error) = self.read_messages(now, on_market) {
            self.drop_connection(now, &error);
        }
        if self.client.is_some() && now >= self.received_last_ms + RECEIVE_SILENCE_MS_MAX {
            self.drop_connection(now, &NetError::Timeout);
        }
        self.engine.tick(now);
        if let Err(error) = self.write_requests(now) {
            self.drop_connection(now, &error);
        }
    }

    /// The one clock the engine sees: wall time at startup, advanced by the monotonic clock. A step
    /// in the wall clock cannot reach a timer. It does stop new orders, because it means the anchor
    /// was wrong, or is now, and only a restart can take a fresh reading.
    fn now(&mut self) -> u64 {
        let (now, stepped) = self.clock.read();
        if stepped {
            self.engine.halt_for_clock_step();
        }
        now
    }

    fn connect(&mut self, now: u64) {
        self.connect_next_ms = now + self.reconnect_backoff_ms;
        self.reconnect_backoff_ms = (self.reconnect_backoff_ms * 2).min(RECONNECT_BACKOFF_MS_MAX);
        let host = self.config.host;
        let client = self.connector.connect(host).and_then(|stream| {
            WsClient::upgrade(stream, host, "/ws", MESSAGE_SIZE_MAX, CONNECT_TIMEOUT_MS)
        });
        match client {
            Ok(client) => {
                self.client = Some(client);
                self.connects += 1;
                self.ping_next_ms = now + u64::from(WS_PING_INTERVAL_MS);
                self.received_last_ms = now;
                if self.subscribe_account().is_err() {
                    self.client = None;
                    return;
                }
                self.engine.on_feed_reconnected(now);
            }
            Err(_) => self.client = None,
        }
    }

    fn subscribe_account(&mut self) -> Result<(), NetError> {
        let user = self.config.user;
        for subscription in [
            Subscription::OrderUpdates { user },
            Subscription::UserFills { user, aggregate_by_time: false },
        ] {
            self.send_walk(&SubscribeMessage { subscription, subscribe: true })?;
        }
        Ok(())
    }

    /// Subscribes to a feed on this session's socket, if it is connected.
    pub fn subscribe(&mut self, subscription: Subscription<'_>) -> Result<(), NetError> {
        self.send_walk(&SubscribeMessage { subscription, subscribe: true })
    }

    fn send_walk<W: Walk>(&mut self, value: &W) -> Result<(), NetError> {
        let Some(client) = self.client.as_mut() else {
            return Err(NetError::Closed);
        };
        let mut sink = JsonSink::new(&mut self.outgoing[CLIENT_HEADER_SIZE_MAX..]);
        value.walk(&mut sink);
        let size = sink.finish().map_err(|_| NetError::BufferTooSmall)?;
        client.send_text(&mut self.outgoing, size)
    }

    fn read_messages(
        &mut self,
        now: u64,
        on_market: &mut dyn FnMut(&Message<'_, '_>),
    ) -> Result<(), NetError> {
        for _ in 0..MESSAGES_PER_STEP_MAX {
            let Some(client) = self.client.as_mut() else {
                return Ok(());
            };
            let (start, size) = match client.poll()? {
                WsEvent::Idle => return Ok(()),
                WsEvent::Message { start, size } => (start, size),
            };
            self.messages += 1;
            self.received_last_ms = now;
            // A connection that delivers is a connection worth having come back to.
            self.reconnect_backoff_ms = RECONNECT_BACKOFF_MS_MIN;
            let bytes = client.message(start, size);
            // A message that will not parse is an operating error: it is dropped, and the reconcile
            // covers anything it carried.
            let Ok(document) = parse(bytes, &mut self.tokens) else {
                continue;
            };
            let Some(message) = Message::parse(document.root()) else {
                continue;
            };
            if message.channel == Channel::Post {
                deliver_post(&mut self.engine, &mut self.inflight, now, &message);
            } else if !self.engine.on_message(now, &message) {
                on_market(&message);
            }
        }
        Ok(())
    }

    fn write_requests(&mut self, now: u64) -> Result<(), NetError> {
        if self.client.is_none() {
            return Ok(());
        }
        // The engine refuses when its request slots are full, which bounds this loop.
        for _ in 0..REQUESTS_MAX {
            let polled = self.engine.poll_transmit(now, &self.signer, &mut self.request);
            let Some(transmit) = polled else {
                break;
            };
            note_inflight(&mut self.inflight, transmit.request_id);
            let kind = match transmit.endpoint {
                Endpoint::Exchange => "action",
                Endpoint::Info => "info",
            };
            self.send_post(transmit.request_id, kind, transmit.size)?;
        }
        if now >= self.ping_next_ms {
            self.ping_next_ms = now + u64::from(WS_PING_INTERVAL_MS);
            self.send_walk(&PingMessage)?;
        }
        Ok(())
    }

    /// Wraps an already-signed body in a post by concatenating bytes, so that the body the
    /// signature covers is sent exactly as it was written.
    fn send_post(&mut self, id: u64, kind: &str, body_size: usize) -> Result<(), NetError> {
        let Some(client) = self.client.as_mut() else {
            return Err(NetError::Closed);
        };
        let target = &mut self.outgoing[CLIENT_HEADER_SIZE_MAX..];
        let mut size = 0usize;
        let mut digits = [0u8; 20];
        let id_text = decimal(id, &mut digits);
        for part in [
            b"{\"method\":\"post\",\"id\":".as_slice(),
            id_text,
            b",\"request\":{\"type\":\"",
            kind.as_bytes(),
            b"\",\"payload\":",
            &self.request[..body_size],
            b"}}",
        ] {
            target[size..size + part.len()].copy_from_slice(part);
            size += part.len();
        }
        assert!(size <= POST_WRAPPER_SIZE_MAX + REQUEST_SIZE_MAX);
        client.send_text(&mut self.outgoing, size)
    }

    /// The connection is gone. Whatever was on it may or may not have landed, and the engine knows
    /// what to do about that.
    fn drop_connection(&mut self, now: u64, _error: &NetError) {
        self.client = None;
        self.connect_next_ms = self.connect_next_ms.max(now + self.reconnect_backoff_ms);
        for id in &mut self.inflight {
            if *id != 0 {
                self.engine.on_reply(now, *id, Reply::Failed);
                *id = 0;
            }
        }
    }

    /// The oldest event not yet taken.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.engine.poll_event()
    }
}

const _: () = assert!(EVENTS_MAX > 0);

/// Wall time as read once at startup, advanced by the monotonic clock.
#[derive(Clone, Copy, Debug)]
struct AnchoredClock<K: Clock> {
    clock: K,
    wall_at_start_ms: u64,
    monotonic_at_start_ms: u64,
}

impl<K: Clock> AnchoredClock<K> {
    fn new(clock: K) -> AnchoredClock<K> {
        let wall_at_start_ms = clock.wall_ms();
        let monotonic_at_start_ms = clock.monotonic_ms();
        AnchoredClock { clock, wall_at_start_ms, monotonic_at_start_ms }
    }

    /// The time, and whether the wall clock has parted from it by more than a slew could explain.
    fn read(&self) -> (u64, bool) {
        let elapsed = self.clock.monotonic_ms().saturating_sub(self.monotonic_at_start_ms);
        let now = self.wall_at_start_ms.saturating_add(elapsed);
        (now, self.clock.wall_ms().abs_diff(now) > CLOCK_STEP_MS_MAX)
    }
}

fn decimal(value: u64, digits: &mut [u8; 20]) -> &[u8] {
    let mut position = digits.len();
    let mut remaining = value;
    for _ in 0..digits.len() {
        position -= 1;
        digits[position] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    assert_eq!(remaining, 0);
    &digits[position..]
}

fn note_inflight(inflight: &mut [u64; REQUESTS_MAX], id: u64) {
    assert!(id != 0);
    // The engine holds at most this many requests, and it has not reported this one's end, so a
    // free entry exists unless a reply went unmatched. Reusing the oldest then costs only a fast
    // failure notice: the engine's own timeout still covers that request.
    let position = inflight.iter().position(|entry| *entry == 0);
    let oldest = inflight.iter().enumerate().min_by_key(|(_, entry)| **entry).map(|(at, _)| at);
    match position.or(oldest) {
        Some(at) => inflight[at] = id,
        None => panic!("REQUESTS_MAX is not zero"),
    }
}

fn deliver_post<const ORDERS: usize, const MARKETS: usize>(
    engine: &mut Engine<ORDERS, MARKETS>,
    inflight: &mut [u64; REQUESTS_MAX],
    now: u64,
    message: &Message<'_, '_>,
) {
    let Some(reply) = message.data.and_then(PostReply::parse) else {
        return;
    };
    if let Some(entry) = inflight.iter_mut().find(|entry| **entry == reply.id) {
        *entry = 0;
    }
    match reply.kind {
        PostKind::Action => engine.on_reply(now, reply.id, Reply::Value(reply.payload)),
        PostKind::Info => match reply.payload.get("data") {
            Some(data) => engine.on_reply(now, reply.id, Reply::Value(data)),
            None => engine.on_reply(now, reply.id, Reply::Failed),
        },
        PostKind::Error => {
            // The status code leads the text, per the docs. A 500 that happens to mention those
            // digits may have landed, and treating it as a definite no would free live exposure.
            let throttled = reply.payload.str().is_some_and(|text| text.starts_with("429"));
            let verdict = if throttled { Reply::Throttled } else { Reply::Failed };
            engine.on_reply(now, reply.id, verdict);
        }
    }
}
