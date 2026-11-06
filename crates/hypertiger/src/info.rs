//! Everything that can be asked: `/info` requests and WebSocket subscriptions, with their weights.

use crate::action::OrderRef;
use crate::sink::{Sink, Walk};
use crate::types::Address;

/// Which request. The tag names the docs' `type` field; arguments follow in the docs' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Info<'a> {
    AllMids {
        dex: Option<&'a str>,
    },
    Meta {
        dex: Option<&'a str>,
    },
    MetaAndAssetCtxs,
    SpotMeta,
    SpotMetaAndAssetCtxs,
    PerpDexs,
    ExchangeStatus,
    L2Book {
        coin: &'a str,
        significant_figures: Option<u32>,
        mantissa: Option<u32>,
    },
    CandleSnapshot {
        coin: &'a str,
        interval: &'a str,
        time_start_ms: u64,
        time_end_ms: u64,
    },
    FundingHistory {
        coin: &'a str,
        time_start_ms: u64,
        time_end_ms: Option<u64>,
    },
    RecentTrades {
        coin: &'a str,
    },
    PerpsAtOpenInterestCap,
    PredictedFundings,
    OutcomeMeta,
    ClearinghouseState {
        user: Address,
        dex: Option<&'a str>,
    },
    SpotClearinghouseState {
        user: Address,
    },
    OpenOrders {
        user: Address,
        dex: Option<&'a str>,
    },
    FrontendOpenOrders {
        user: Address,
        dex: Option<&'a str>,
    },
    HistoricalOrders {
        user: Address,
    },
    OrderStatus {
        user: Address,
        target: OrderRef,
    },
    UserFills {
        user: Address,
        aggregate_by_time: bool,
    },
    UserFillsByTime {
        user: Address,
        time_start_ms: u64,
        time_end_ms: Option<u64>,
        aggregate_by_time: bool,
    },
    UserFunding {
        user: Address,
        time_start_ms: u64,
        time_end_ms: Option<u64>,
    },
    UserNonFundingLedgerUpdates {
        user: Address,
        time_start_ms: u64,
        time_end_ms: Option<u64>,
    },
    UserRateLimit {
        user: Address,
    },
    UserFees {
        user: Address,
    },
    UserRole {
        user: Address,
    },
    UserVaultEquities {
        user: Address,
    },
    UserTwapSliceFills {
        user: Address,
    },
    UserAbstraction {
        user: Address,
    },
    UserDexAbstraction {
        user: Address,
    },
    UserToMultiSigSigners {
        user: Address,
    },
    SubAccounts {
        user: Address,
    },
    ExtraAgents {
        user: Address,
    },
    Portfolio {
        user: Address,
    },
    Referral {
        user: Address,
    },
    Delegations {
        user: Address,
    },
    DelegatorSummary {
        user: Address,
    },
    DelegatorHistory {
        user: Address,
    },
    DelegatorRewards {
        user: Address,
    },
    ActiveAssetData {
        user: Address,
        coin: &'a str,
    },
    VaultDetails {
        vault: Address,
        user: Option<Address>,
    },
    MaxBuilderFee {
        user: Address,
        builder: Address,
    },
    /// Any request with no arguments that this version does not name.
    Bare {
        kind: &'a str,
    },
    /// Any request with only a user that this version does not name.
    ForUser {
        kind: &'a str,
        user: Address,
    },
}

impl Info<'_> {
    /// The base weight per the docs' rate-limit page, for the budget.
    ///
    /// Some requests add weight per item returned, which is only known afterward; a client that
    /// pages through history should charge that from the response.
    #[must_use]
    pub const fn weight(&self) -> u32 {
        match self {
            Info::AllMids { .. }
            | Info::L2Book { .. }
            | Info::ClearinghouseState { .. }
            | Info::OrderStatus { .. }
            | Info::SpotClearinghouseState { .. }
            | Info::ExchangeStatus => 2,
            Info::UserRole { .. } => 60,
            _ => 20,
        }
    }
}

const _: () = assert!(Info::ExchangeStatus.weight() == 2);
const _: () = assert!(Info::PerpDexs.weight() == 20);

fn tag<S: Sink>(sink: &mut S, entries: u32, kind: &str) {
    assert!(entries > 0);
    assert_ne!(kind, "");
    sink.map(entries);
    sink.key("type");
    sink.str(kind);
}

fn entry_user<S: Sink>(sink: &mut S, key: &str, user: Address) {
    sink.key(key);
    sink.str(user.to_hex().as_str());
}

fn entry_str<S: Sink>(sink: &mut S, key: &str, value: &str) {
    sink.key(key);
    sink.str(value);
}

fn entry_u64<S: Sink>(sink: &mut S, key: &str, value: u64) {
    sink.key(key);
    sink.u64(value);
}

fn only_dex<S: Sink>(sink: &mut S, kind: &str, dex: Option<&str>) {
    tag(sink, if dex.is_some() { 2 } else { 1 }, kind);
    if let Some(dex) = dex {
        entry_str(sink, "dex", dex);
    }
}

fn user_dex<S: Sink>(sink: &mut S, kind: &str, user: Address, dex: Option<&str>) {
    tag(sink, if dex.is_some() { 3 } else { 2 }, kind);
    entry_user(sink, "user", user);
    if let Some(dex) = dex {
        entry_str(sink, "dex", dex);
    }
}

fn only_user<S: Sink>(sink: &mut S, kind: &str, user: Address) {
    tag(sink, 2, kind);
    entry_user(sink, "user", user);
}

fn user_range<S: Sink>(sink: &mut S, kind: &str, user: Address, start: u64, end: Option<u64>) {
    tag(sink, if end.is_some() { 4 } else { 3 }, kind);
    entry_user(sink, "user", user);
    entry_u64(sink, "startTime", start);
    if let Some(end) = end {
        entry_u64(sink, "endTime", end);
    }
}

impl Walk for Info<'_> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        match *self {
            Info::AllMids { dex } => only_dex(sink, "allMids", dex),
            Info::Meta { dex } => only_dex(sink, "meta", dex),
            Info::MetaAndAssetCtxs => tag(sink, 1, "metaAndAssetCtxs"),
            Info::SpotMeta => tag(sink, 1, "spotMeta"),
            Info::SpotMetaAndAssetCtxs => tag(sink, 1, "spotMetaAndAssetCtxs"),
            Info::PerpDexs => tag(sink, 1, "perpDexs"),
            Info::ExchangeStatus => tag(sink, 1, "exchangeStatus"),
            Info::PerpsAtOpenInterestCap => tag(sink, 1, "perpsAtOpenInterestCap"),
            Info::PredictedFundings => tag(sink, 1, "predictedFundings"),
            Info::OutcomeMeta => tag(sink, 1, "outcomeMeta"),
            Info::Bare { kind } => tag(sink, 1, kind),
            Info::L2Book { .. }
            | Info::CandleSnapshot { .. }
            | Info::FundingHistory { .. }
            | Info::RecentTrades { .. } => walk_market(self, sink),
            _ => walk_user(self, sink),
        }
    }
}

fn walk_market<S: Sink>(request: &Info<'_>, sink: &mut S) {
    match *request {
        Info::L2Book { coin, significant_figures, mantissa } => {
            // The docs: `mantissa` is only allowed alongside `nSigFigs` of 5.
            assert!(mantissa.is_none() || significant_figures == Some(5));
            let entries =
                2 + u32::from(significant_figures.is_some()) + u32::from(mantissa.is_some());
            tag(sink, entries, "l2Book");
            entry_str(sink, "coin", coin);
            if let Some(figures) = significant_figures {
                entry_u64(sink, "nSigFigs", u64::from(figures));
            }
            if let Some(mantissa) = mantissa {
                entry_u64(sink, "mantissa", u64::from(mantissa));
            }
        }
        Info::CandleSnapshot { coin, interval, time_start_ms, time_end_ms } => {
            assert!(time_start_ms <= time_end_ms);
            tag(sink, 2, "candleSnapshot");
            sink.key("req");
            sink.map(4);
            entry_str(sink, "coin", coin);
            entry_str(sink, "interval", interval);
            entry_u64(sink, "startTime", time_start_ms);
            entry_u64(sink, "endTime", time_end_ms);
        }
        Info::FundingHistory { coin, time_start_ms, time_end_ms } => {
            tag(sink, if time_end_ms.is_some() { 4 } else { 3 }, "fundingHistory");
            entry_str(sink, "coin", coin);
            entry_u64(sink, "startTime", time_start_ms);
            if let Some(end) = time_end_ms {
                entry_u64(sink, "endTime", end);
            }
        }
        Info::RecentTrades { coin } => {
            tag(sink, 2, "recentTrades");
            entry_str(sink, "coin", coin);
        }
        _ => panic!("only market requests are dispatched here"),
    }
}

fn walk_user<S: Sink>(request: &Info<'_>, sink: &mut S) {
    match *request {
        Info::ClearinghouseState { user, dex } => user_dex(sink, "clearinghouseState", user, dex),
        Info::OpenOrders { user, dex } => user_dex(sink, "openOrders", user, dex),
        Info::FrontendOpenOrders { user, dex } => user_dex(sink, "frontendOpenOrders", user, dex),
        Info::OrderStatus { user, target } => {
            tag(sink, 3, "orderStatus");
            entry_user(sink, "user", user);
            sink.key("oid");
            match target {
                OrderRef::Oid(oid) => sink.u64(oid),
                OrderRef::Cloid(cloid) => sink.str(cloid.to_hex().as_str()),
            }
        }
        Info::UserFills { user, aggregate_by_time } => {
            tag(sink, if aggregate_by_time { 3 } else { 2 }, "userFills");
            entry_user(sink, "user", user);
            if aggregate_by_time {
                sink.key("aggregateByTime");
                sink.bool(true);
            }
        }
        Info::UserFillsByTime { user, time_start_ms, time_end_ms, aggregate_by_time } => {
            let entries = 3 + u32::from(time_end_ms.is_some()) + u32::from(aggregate_by_time);
            tag(sink, entries, "userFillsByTime");
            entry_user(sink, "user", user);
            entry_u64(sink, "startTime", time_start_ms);
            if let Some(end) = time_end_ms {
                entry_u64(sink, "endTime", end);
            }
            if aggregate_by_time {
                sink.key("aggregateByTime");
                sink.bool(true);
            }
        }
        Info::UserFunding { user, time_start_ms, time_end_ms } => {
            user_range(sink, "userFunding", user, time_start_ms, time_end_ms);
        }
        Info::UserNonFundingLedgerUpdates { user, time_start_ms, time_end_ms } => {
            user_range(sink, "userNonFundingLedgerUpdates", user, time_start_ms, time_end_ms);
        }
        Info::ActiveAssetData { user, coin } => {
            tag(sink, 3, "activeAssetData");
            entry_user(sink, "user", user);
            entry_str(sink, "coin", coin);
        }
        Info::VaultDetails { vault, user } => {
            tag(sink, if user.is_some() { 3 } else { 2 }, "vaultDetails");
            entry_user(sink, "vaultAddress", vault);
            if let Some(user) = user {
                entry_user(sink, "user", user);
            }
        }
        Info::MaxBuilderFee { user, builder } => {
            tag(sink, 3, "maxBuilderFee");
            entry_user(sink, "user", user);
            entry_user(sink, "builder", builder);
        }
        _ => {
            let (kind, user) = only_user_request(request);
            only_user(sink, kind, user);
        }
    }
}

/// The requests whose only argument is a user: one table, so a new one is one line.
fn only_user_request<'a>(request: &Info<'a>) -> (&'a str, Address) {
    match *request {
        Info::SpotClearinghouseState { user } => ("spotClearinghouseState", user),
        Info::HistoricalOrders { user } => ("historicalOrders", user),
        Info::UserRateLimit { user } => ("userRateLimit", user),
        Info::UserFees { user } => ("userFees", user),
        Info::UserRole { user } => ("userRole", user),
        Info::UserVaultEquities { user } => ("userVaultEquities", user),
        Info::UserTwapSliceFills { user } => ("userTwapSliceFills", user),
        Info::UserAbstraction { user } => ("userAbstraction", user),
        Info::UserDexAbstraction { user } => ("userDexAbstraction", user),
        Info::UserToMultiSigSigners { user } => ("userToMultiSigSigners", user),
        Info::SubAccounts { user } => ("subAccounts", user),
        Info::ExtraAgents { user } => ("extraAgents", user),
        Info::Portfolio { user } => ("portfolio", user),
        Info::Referral { user } => ("referral", user),
        Info::Delegations { user } => ("delegations", user),
        Info::DelegatorSummary { user } => ("delegatorSummary", user),
        Info::DelegatorHistory { user } => ("delegatorHistory", user),
        Info::DelegatorRewards { user } => ("delegatorRewards", user),
        Info::ForUser { kind, user } => (kind, user),
        _ => panic!("every other request is dispatched by walk or walk_user"),
    }
}

/// A WebSocket feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subscription<'a> {
    AllMids { dex: Option<&'a str> },
    L2Book { coin: &'a str, significant_figures: Option<u32>, mantissa: Option<u32>, fast: bool },
    Trades { coin: &'a str },
    Bbo { coin: &'a str },
    Candle { coin: &'a str, interval: &'a str },
    ActiveAssetCtx { coin: &'a str },
    ActiveAssetData { user: Address, coin: &'a str },
    Notification { user: Address },
    WebData3 { user: Address },
    OrderUpdates { user: Address },
    UserEvents { user: Address },
    UserFills { user: Address, aggregate_by_time: bool },
    UserFundings { user: Address },
    UserNonFundingLedgerUpdates { user: Address },
    UserTwapSliceFills { user: Address },
    UserTwapHistory { user: Address },
    OpenOrders { user: Address, dex: Option<&'a str> },
    ClearinghouseState { user: Address, dex: Option<&'a str> },
    TwapStates { user: Address, dex: Option<&'a str> },
    SpotState { user: Address },
    AllDexsClearinghouseState { user: Address },
    AllDexsAssetCtxs,
    FastAssetCtxs,
    OutcomeMetaUpdates,
}

impl Walk for Subscription<'_> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        match *self {
            Subscription::AllMids { dex } => only_dex(sink, "allMids", dex),
            Subscription::L2Book { coin, significant_figures, mantissa, fast } => {
                walk_book_subscription(coin, significant_figures, mantissa, fast, sink);
            }
            Subscription::Trades { coin } => coin_only(sink, "trades", coin),
            Subscription::Bbo { coin } => coin_only(sink, "bbo", coin),
            Subscription::ActiveAssetCtx { coin } => coin_only(sink, "activeAssetCtx", coin),
            Subscription::Candle { coin, interval } => {
                tag(sink, 3, "candle");
                entry_str(sink, "coin", coin);
                entry_str(sink, "interval", interval);
            }
            Subscription::ActiveAssetData { user, coin } => {
                tag(sink, 3, "activeAssetData");
                entry_user(sink, "user", user);
                entry_str(sink, "coin", coin);
            }
            Subscription::UserFills { user, aggregate_by_time } => {
                tag(sink, if aggregate_by_time { 3 } else { 2 }, "userFills");
                entry_user(sink, "user", user);
                if aggregate_by_time {
                    sink.key("aggregateByTime");
                    sink.bool(true);
                }
            }
            Subscription::OpenOrders { user, dex } => user_dex(sink, "openOrders", user, dex),
            Subscription::ClearinghouseState { user, dex } => {
                user_dex(sink, "clearinghouseState", user, dex);
            }
            Subscription::TwapStates { user, dex } => user_dex(sink, "twapStates", user, dex),
            Subscription::AllDexsAssetCtxs => tag(sink, 1, "allDexsAssetCtxs"),
            Subscription::FastAssetCtxs => tag(sink, 1, "fastAssetCtxs"),
            Subscription::OutcomeMetaUpdates => tag(sink, 1, "outcomeMetaUpdates"),
            _ => {
                let (kind, user) = subscription_user(self);
                only_user(sink, kind, user);
            }
        }
    }
}

fn coin_only<S: Sink>(sink: &mut S, kind: &str, coin: &str) {
    assert_ne!(coin, "");
    tag(sink, 2, kind);
    entry_str(sink, "coin", coin);
}

fn walk_book_subscription<S: Sink>(
    coin: &str,
    significant_figures: Option<u32>,
    mantissa: Option<u32>,
    fast: bool,
    sink: &mut S,
) {
    let entries = 2
        + u32::from(significant_figures.is_some())
        + u32::from(mantissa.is_some())
        + u32::from(fast);
    tag(sink, entries, "l2Book");
    entry_str(sink, "coin", coin);
    if let Some(figures) = significant_figures {
        entry_u64(sink, "nSigFigs", u64::from(figures));
    }
    if let Some(mantissa) = mantissa {
        entry_u64(sink, "mantissa", u64::from(mantissa));
    }
    if fast {
        sink.key("fast");
        sink.bool(true);
    }
}

fn subscription_user<'a>(subscription: &Subscription<'a>) -> (&'a str, Address) {
    match *subscription {
        Subscription::Notification { user } => ("notification", user),
        Subscription::WebData3 { user } => ("webData3", user),
        Subscription::OrderUpdates { user } => ("orderUpdates", user),
        Subscription::UserEvents { user } => ("userEvents", user),
        Subscription::UserFundings { user } => ("userFundings", user),
        Subscription::UserNonFundingLedgerUpdates { user } => ("userNonFundingLedgerUpdates", user),
        Subscription::UserTwapSliceFills { user } => ("userTwapSliceFills", user),
        Subscription::UserTwapHistory { user } => ("userTwapHistory", user),
        Subscription::SpotState { user } => ("spotState", user),
        Subscription::AllDexsClearinghouseState { user } => ("allDexsClearinghouseState", user),
        _ => panic!("every other subscription is dispatched by walk"),
    }
}

/// `{"method":"subscribe","subscription":..}`, and its inverse.
#[derive(Clone, Copy, Debug)]
pub struct SubscribeMessage<'a> {
    pub subscription: Subscription<'a>,
    pub subscribe: bool,
}

impl Walk for SubscribeMessage<'_> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        sink.map(2);
        entry_str(sink, "method", if self.subscribe { "subscribe" } else { "unsubscribe" });
        sink.key("subscription");
        self.subscription.walk(sink);
    }
}

/// `{"method":"ping"}`, the heartbeat.
#[derive(Clone, Copy, Debug)]
pub struct PingMessage;

impl Walk for PingMessage {
    fn walk<S: Sink>(&self, sink: &mut S) {
        sink.map(1);
        entry_str(sink, "method", "ping");
    }
}

/// `{"method":"post","id":..,"request":{"type":..,"payload":..}}`: any request over the WebSocket.
#[derive(Clone, Copy, Debug)]
pub struct PostMessage<'a, W: Walk> {
    pub id: u64,
    pub kind: PostRequestKind,
    pub payload: &'a W,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostRequestKind {
    Info,
    Action,
}

impl<W: Walk> Walk for PostMessage<'_, W> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        sink.map(3);
        entry_str(sink, "method", "post");
        entry_u64(sink, "id", self.id);
        sink.key("request");
        sink.map(2);
        entry_str(
            sink,
            "type",
            match self.kind {
                PostRequestKind::Info => "info",
                PostRequestKind::Action => "action",
            },
        );
        sink.key("payload");
        self.payload.walk(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::JsonSink;
    use crate::types::Cloid;

    fn json<W: Walk>(value: &W) -> std::string::String {
        let mut buffer = [0u8; 1024];
        let mut sink = JsonSink::new(&mut buffer);
        value.walk(&mut sink);
        let size = sink.finish().unwrap();
        std::string::String::from_utf8(buffer[..size].to_vec()).unwrap()
    }

    const USER: Address = Address([0x11; 20]);
    const HEX: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn requests_take_their_documented_forms() {
        assert_eq!(json(&Info::AllMids { dex: None }), r#"{"type":"allMids"}"#);
        assert_eq!(json(&Info::AllMids { dex: Some("xyz") }), r#"{"type":"allMids","dex":"xyz"}"#);
        assert_eq!(
            json(&Info::L2Book { coin: "BTC", significant_figures: Some(5), mantissa: Some(2) }),
            r#"{"type":"l2Book","coin":"BTC","nSigFigs":5,"mantissa":2}"#
        );
        assert_eq!(
            json(&Info::CandleSnapshot {
                coin: "BTC",
                interval: "1m",
                time_start_ms: 1,
                time_end_ms: 2
            }),
            r#"{"type":"candleSnapshot","req":{"coin":"BTC","interval":"1m","startTime":1,"endTime":2}}"#
        );
        assert_eq!(
            json(&Info::UserFillsByTime {
                user: USER,
                time_start_ms: 5,
                time_end_ms: None,
                aggregate_by_time: true
            }),
            std::format!(
                r#"{{"type":"userFillsByTime","user":"{HEX}","startTime":5,"aggregateByTime":true}}"#
            )
        );
        assert_eq!(
            json(&Info::OrderStatus { user: USER, target: OrderRef::Oid(7) }),
            std::format!(r#"{{"type":"orderStatus","user":"{HEX}","oid":7}}"#)
        );
        let cloid = Cloid::from_parts(0, 1);
        assert_eq!(
            json(&Info::OrderStatus { user: USER, target: OrderRef::Cloid(cloid) }),
            std::format!(
                r#"{{"type":"orderStatus","user":"{HEX}","oid":"0x00000000000000000000000000000001"}}"#
            )
        );
        assert_eq!(
            json(&Info::UserRateLimit { user: USER }),
            std::format!(r#"{{"type":"userRateLimit","user":"{HEX}"}}"#)
        );
    }

    #[test]
    fn weights_follow_the_rate_limit_page() {
        assert_eq!(
            Info::L2Book { coin: "B", significant_figures: None, mantissa: None }.weight(),
            2
        );
        assert_eq!(Info::OrderStatus { user: USER, target: OrderRef::Oid(1) }.weight(), 2);
        assert_eq!(Info::UserRole { user: USER }.weight(), 60);
        assert_eq!(Info::Meta { dex: None }.weight(), 20);
        assert_eq!(Info::FrontendOpenOrders { user: USER, dex: None }.weight(), 20);
    }

    #[test]
    fn websocket_messages_take_their_documented_forms() {
        assert_eq!(json(&PingMessage), r#"{"method":"ping"}"#);
        let trades = Subscription::Trades { coin: "SOL" };
        assert_eq!(
            json(&SubscribeMessage { subscription: trades, subscribe: true }),
            r#"{"method":"subscribe","subscription":{"type":"trades","coin":"SOL"}}"#
        );
        assert_eq!(
            json(&SubscribeMessage { subscription: trades, subscribe: false }),
            r#"{"method":"unsubscribe","subscription":{"type":"trades","coin":"SOL"}}"#
        );
        let post =
            PostMessage { id: 256, kind: PostRequestKind::Info, payload: &Info::ExchangeStatus };
        assert_eq!(
            json(&post),
            r#"{"method":"post","id":256,"request":{"type":"info","payload":{"type":"exchangeStatus"}}}"#
        );
        assert_eq!(
            json(&Subscription::L2Book {
                coin: "BTC",
                significant_figures: None,
                mantissa: None,
                fast: true
            }),
            r#"{"type":"l2Book","coin":"BTC","fast":true}"#
        );
        assert_eq!(
            json(&Subscription::UserEvents { user: USER }),
            std::format!(r#"{{"type":"userEvents","user":"{HEX}"}}"#)
        );
    }

    #[test]
    #[should_panic(expected = "mantissa")]
    fn a_mantissa_without_five_figures_is_a_bug() {
        let _ = json(&Info::L2Book { coin: "B", significant_figures: Some(4), mantissa: Some(2) });
    }
}
