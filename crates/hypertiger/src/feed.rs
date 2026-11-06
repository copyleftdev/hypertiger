//! What the exchange sends: WebSocket messages, `/info` results and `/exchange` replies.
//!
//! Every type here reads from a parsed document and copies only fixed-size values. Shapes come from
//! captured traffic (`tests/fixtures`), not only the docs, which they differ from: fills carry a
//! `cloid` the docs omit, and many fields the docs type as numbers arrive as strings.
//!
//! Nothing from the network is asserted on. A malformed field is an operating error, and it is
//! returned as `None` so that the caller decides what a missing field means.

use crate::decimal::{Decimal, Px, Sz, Szi};
use crate::json::Value;
use crate::types::{Address, Cloid, Coin, Side};

/// Which stream a WebSocket message belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Pong,
    SubscriptionResponse,
    Post,
    Error,
    AllMids,
    L2Book,
    Bbo,
    Trades,
    Candle,
    ActiveAssetCtx,
    ActiveSpotAssetCtx,
    ActiveAssetData,
    OrderUpdates,
    UserFills,
    UserEvents,
    UserFundings,
    UserNonFundingLedgerUpdates,
    UserTwapSliceFills,
    UserTwapHistory,
    OpenOrders,
    ClearinghouseState,
    SpotState,
    Notification,
    WebData,
    /// A channel this version does not name. Reported and not dropped, because new ones appear.
    Other,
}

impl Channel {
    fn parse(name: &str) -> Channel {
        match name {
            "pong" => Channel::Pong,
            "subscriptionResponse" => Channel::SubscriptionResponse,
            "post" => Channel::Post,
            "error" => Channel::Error,
            "allMids" => Channel::AllMids,
            "l2Book" => Channel::L2Book,
            "bbo" => Channel::Bbo,
            "trades" => Channel::Trades,
            "candle" => Channel::Candle,
            "activeAssetCtx" => Channel::ActiveAssetCtx,
            "activeSpotAssetCtx" => Channel::ActiveSpotAssetCtx,
            "activeAssetData" => Channel::ActiveAssetData,
            "orderUpdates" => Channel::OrderUpdates,
            "userFills" => Channel::UserFills,
            // The docs: the channel name for `userEvents` messages is `user`.
            "user" => Channel::UserEvents,
            "userFundings" => Channel::UserFundings,
            "userNonFundingLedgerUpdates" => Channel::UserNonFundingLedgerUpdates,
            "userTwapSliceFills" => Channel::UserTwapSliceFills,
            "userTwapHistory" => Channel::UserTwapHistory,
            "openOrders" => Channel::OpenOrders,
            "clearinghouseState" => Channel::ClearinghouseState,
            "spotState" => Channel::SpotState,
            "notification" => Channel::Notification,
            "webData2" | "webData3" => Channel::WebData,
            _ => Channel::Other,
        }
    }
}

/// One WebSocket message: its stream and its payload, not yet read.
#[derive(Clone, Copy, Debug)]
pub struct Message<'s, 't> {
    pub channel: Channel,
    pub channel_name: &'s str,
    pub data: Option<Value<'s, 't>>,
}

impl<'s, 't> Message<'s, 't> {
    /// `None` when the root is not a message at all.
    #[must_use]
    pub fn parse(root: Value<'s, 't>) -> Option<Message<'s, 't>> {
        let channel_name = root.get("channel")?.str()?;
        Some(Message {
            channel: Channel::parse(channel_name),
            channel_name,
            data: root.get("data"),
        })
    }

    /// Whether this is a snapshot replay, which a consumer must not count as new events.
    #[must_use]
    pub fn is_snapshot(&self) -> bool {
        let snapshot = self.data.and_then(|data| data.get("isSnapshot"));
        snapshot.and_then(Value::bool).unwrap_or(false)
    }
}

/// One side's price level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub px: Px,
    pub sz: Sz,
    pub orders_count: u32,
}

impl Level {
    pub const ZERO: Level = Level { px: Px::ZERO, sz: Sz::ZERO, orders_count: 0 };

    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<Level> {
        Some(Level {
            px: Px::parse(value.get("px")?.text()?).ok()?,
            sz: Sz::parse(value.get("sz")?.text()?).ok()?,
            orders_count: u32::try_from(value.get("n")?.u64()?).ok()?,
        })
    }
}

/// The best bid and ask. Either side can be empty, which the feed sends as `null`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bbo {
    pub coin: Coin,
    pub time_ms: u64,
    pub bid: Option<Level>,
    pub ask: Option<Level>,
}

impl Bbo {
    #[must_use]
    pub fn parse(data: Value<'_, '_>) -> Option<Bbo> {
        let mut sides = data.get("bbo")?.items();
        let bid = sides.next()?;
        let ask = sides.next()?;
        Some(Bbo {
            coin: Coin::parse(data.get("coin")?.text()?)?,
            time_ms: data.get("time")?.u64()?,
            bid: if bid.is_null() { None } else { Some(Level::parse(bid)?) },
            ask: if ask.is_null() { None } else { Some(Level::parse(ask)?) },
        })
    }
}

/// One public trade. `side` is the aggressor's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trade {
    pub coin: Coin,
    pub side: Side,
    pub px: Px,
    pub sz: Sz,
    pub time_ms: u64,
    /// A 50-bit hash of the two order ids. The docs: `(time, coin, tid)` is the unique key.
    pub tid: u64,
}

impl Trade {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<Trade> {
        Some(Trade {
            coin: Coin::parse(value.get("coin")?.text()?)?,
            side: Side::parse(value.get("side")?.text()?)?,
            px: Px::parse(value.get("px")?.text()?).ok()?,
            sz: Sz::parse(value.get("sz")?.text()?).ok()?,
            time_ms: value.get("time")?.u64()?,
            tid: value.get("tid")?.u64()?,
        })
    }
}

/// One fill of one of the user's orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub coin: Coin,
    pub side: Side,
    pub px: Px,
    pub sz: Sz,
    pub time_ms: u64,
    pub oid: u64,
    pub tid: u64,
    /// Present on the wire though absent from the docs; `None` for an order placed without one.
    pub cloid: Option<Cloid>,
    pub crossed: bool,
    pub fee: Decimal,
    pub closed_pnl: Decimal,
    /// The account's position just before this fill, from the exchange. It is what lets a tracked
    /// position correct itself: any later fill restates the truth.
    pub start_position: Option<Szi>,
}

impl Fill {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<Fill> {
        Some(Fill {
            coin: Coin::parse(value.get("coin")?.text()?)?,
            side: Side::parse(value.get("side")?.text()?)?,
            px: Px::parse(value.get("px")?.text()?).ok()?,
            sz: Sz::parse(value.get("sz")?.text()?).ok()?,
            time_ms: value.get("time")?.u64()?,
            oid: value.get("oid")?.u64()?,
            tid: value.get("tid")?.u64()?,
            cloid: parse_cloid(value.get("cloid")),
            crossed: value.get("crossed").and_then(Value::bool).unwrap_or(false),
            fee: read_decimal(value.get("fee")),
            closed_pnl: read_decimal(value.get("closedPnl")),
            start_position: value
                .get("startPosition")
                .and_then(Value::text)
                .and_then(|text| Szi::parse(text).ok()),
        })
    }
}

/// An absent or odd accounting field reads as zero: it must not lose the fill it came with, which
/// moves the position and matters far more.
fn read_decimal(value: Option<Value<'_, '_>>) -> Decimal {
    value.and_then(Value::text).and_then(|text| Decimal::parse(text).ok()).unwrap_or(Decimal::ZERO)
}

fn parse_cloid(value: Option<Value<'_, '_>>) -> Option<Cloid> {
    Cloid::parse(value?.text()?)
}

/// What an order's status string means for the money.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusClass {
    /// On the book, or armed: `open`, `triggered`.
    Live,
    /// Entirely filled.
    Filled,
    /// Finished with whatever had filled so far: every `*Canceled`, every `*Rejected`, and
    /// `scheduledCancel`.
    Dead,
    /// A status this version does not know. Never assumed dead, because that would free exposure
    /// on a word nobody has read.
    Unknown,
}

impl StatusClass {
    #[must_use]
    pub fn parse(status: &str) -> StatusClass {
        match status {
            "open" | "triggered" => StatusClass::Live,
            "filled" => StatusClass::Filled,
            "canceled" | "rejected" | "scheduledCancel" => StatusClass::Dead,
            _ if status.ends_with("Canceled") || status.ends_with("Rejected") => StatusClass::Dead,
            _ => StatusClass::Unknown,
        }
    }
}

/// The order fields shared by `orderUpdates`, `orderStatus` and `frontendOpenOrders`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderView {
    pub coin: Coin,
    pub side: Side,
    pub limit_px: Px,
    /// What remains open.
    pub sz: Sz,
    pub sz_orig: Sz,
    pub oid: u64,
    pub time_ms: u64,
    pub cloid: Option<Cloid>,
    pub reduce_only: bool,
    /// A TP/SL. A fast cancel cannot reach one, per the docs.
    pub is_trigger: bool,
}

impl OrderView {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<OrderView> {
        Some(OrderView {
            coin: Coin::parse(value.get("coin")?.text()?)?,
            side: Side::parse(value.get("side")?.text()?)?,
            limit_px: Px::parse(value.get("limitPx")?.text()?).ok()?,
            sz: Sz::parse(value.get("sz")?.text()?).ok()?,
            sz_orig: Sz::parse(value.get("origSz")?.text()?).ok()?,
            oid: value.get("oid")?.u64()?,
            time_ms: value.get("timestamp")?.u64()?,
            cloid: parse_cloid(value.get("cloid")),
            reduce_only: value.get("reduceOnly").and_then(Value::bool).unwrap_or(false),
            is_trigger: value.get("isTrigger").and_then(Value::bool).unwrap_or(false),
        })
    }
}

/// An order and what has happened to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderUpdate {
    pub order: OrderView,
    pub class: StatusClass,
    pub status_time_ms: u64,
}

impl OrderUpdate {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<OrderUpdate> {
        Some(OrderUpdate {
            order: OrderView::parse(value.get("order")?)?,
            class: StatusClass::parse(value.get("status")?.str()?),
            status_time_ms: value.get("statusTimestamp")?.u64()?,
        })
    }
}

/// The `orderStatus` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatusReply {
    /// The exchange has never seen this id, as far as this API server knows. See `docs/DESIGN.md`
    /// for when that alone can be believed.
    Unknown,
    Found(OrderUpdate),
}

impl OrderStatusReply {
    #[must_use]
    pub fn parse(root: Value<'_, '_>) -> Option<OrderStatusReply> {
        match root.get("status")?.str()? {
            "unknownOid" => Some(OrderStatusReply::Unknown),
            "order" => Some(OrderStatusReply::Found(OrderUpdate::parse(root.get("order")?)?)),
            _ => None,
        }
    }
}

/// One perp position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub coin: Coin,
    pub szi: Szi,
    pub entry_px: Option<Px>,
    pub unrealized_pnl: Decimal,
    pub liquidation_px: Option<Px>,
    pub margin_used: Decimal,
}

impl Position {
    /// Reads one element of `assetPositions`.
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<Position> {
        let inner = value.get("position")?;
        Some(Position {
            coin: Coin::parse(inner.get("coin")?.text()?)?,
            szi: Szi::parse(inner.get("szi")?.text()?).ok()?,
            entry_px: read_px(inner.get("entryPx")),
            unrealized_pnl: read_decimal(inner.get("unrealizedPnl")),
            // Liquidation prices carry 10 decimals on the wire, past what `Px` holds, so this is
            // cut to 8 for display. It is advice, never a value an order is built from.
            liquidation_px: read_px_truncated(inner.get("liquidationPx")),
            margin_used: read_decimal(inner.get("marginUsed")),
        })
    }
}

fn read_px(value: Option<Value<'_, '_>>) -> Option<Px> {
    Px::parse(value?.text()?).ok()
}

fn read_px_truncated(value: Option<Value<'_, '_>>) -> Option<Px> {
    let raw = Decimal::parse(value?.text()?).ok()?.raw();
    if raw < 0 {
        return None;
    }
    let cut = raw.div_euclid(100);
    Some(Px::from_raw(u64::try_from(cut).ok()?))
}

/// A perp account summary, from `clearinghouseState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountSummary {
    pub account_value: Decimal,
    pub margin_used: Decimal,
    pub withdrawable: Decimal,
    pub time_ms: u64,
}

impl AccountSummary {
    #[must_use]
    pub fn parse(root: Value<'_, '_>) -> Option<AccountSummary> {
        let summary = root.get("marginSummary")?;
        Some(AccountSummary {
            account_value: Decimal::parse(summary.get("accountValue")?.text()?).ok()?,
            margin_used: Decimal::parse(summary.get("totalMarginUsed")?.text()?).ok()?,
            withdrawable: Decimal::parse(root.get("withdrawable")?.text()?).ok()?,
            time_ms: root.get("time")?.u64()?,
        })
    }
}

/// Perp market context, from `activeAssetCtx` and `metaAndAssetCtxs`. Fields are optional because
/// an asset that has not traded omits some of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetContext {
    pub mark_px: Option<Px>,
    pub mid_px: Option<Px>,
    pub oracle_px: Option<Px>,
    pub funding: Option<Decimal>,
    pub open_interest: Option<Sz>,
}

impl AssetContext {
    #[must_use]
    pub fn parse(context: Value<'_, '_>) -> AssetContext {
        AssetContext {
            mark_px: read_px(context.get("markPx")),
            mid_px: read_px(context.get("midPx")),
            oracle_px: read_px(context.get("oraclePx")),
            funding: context
                .get("funding")
                .and_then(Value::text)
                .and_then(|text| Decimal::parse(text).ok()),
            // Open interest carries up to 10 decimals, which `Sz` refuses unless they are zeros.
            open_interest: context
                .get("openInterest")
                .and_then(Value::text)
                .and_then(|text| Sz::parse(text).ok()),
        }
    }
}

/// One candle. The feed sends prices as strings where the docs say numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candle {
    pub time_open_ms: u64,
    pub time_close_ms: u64,
    pub coin: Coin,
    pub open: Px,
    pub close: Px,
    pub high: Px,
    pub low: Px,
    pub volume: Sz,
    pub trades_count: u64,
}

impl Candle {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<Candle> {
        Some(Candle {
            time_open_ms: value.get("t")?.u64()?,
            time_close_ms: value.get("T")?.u64()?,
            coin: Coin::parse(value.get("s")?.text()?)?,
            open: Px::parse(value.get("o")?.text()?).ok()?,
            close: Px::parse(value.get("c")?.text()?).ok()?,
            high: Px::parse(value.get("h")?.text()?).ok()?,
            low: Px::parse(value.get("l")?.text()?).ok()?,
            volume: Sz::parse(value.get("v")?.text()?).ok()?,
            trades_count: value.get("n")?.u64()?,
        })
    }
}

/// The address-based limit, from `userRateLimit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserRateLimit {
    pub requests_used: u64,
    pub requests_cap: u64,
    pub requests_surplus: u64,
}

impl UserRateLimit {
    #[must_use]
    pub fn parse(root: Value<'_, '_>) -> Option<UserRateLimit> {
        Some(UserRateLimit {
            requests_used: root.get("nRequestsUsed")?.u64()?,
            requests_cap: root.get("nRequestsCap")?.u64()?,
            requests_surplus: root.get("nRequestsSurplus").and_then(Value::u64).unwrap_or(0),
        })
    }
}

/// One entry of an `/exchange` reply's `statuses`, for placing an order.
#[derive(Clone, Copy, Debug)]
pub enum PlaceStatus<'s> {
    Resting {
        oid: u64,
        cloid: Option<Cloid>,
    },
    Filled {
        total_sz: Sz,
        avg_px: Px,
        oid: u64,
        cloid: Option<Cloid>,
    },
    /// The order was refused, for this reason, and is not on the book.
    Error(&'s str),
    /// A shape this version does not read. Treated as an unknown outcome, never as a rejection.
    Unrecognized,
}

impl<'s> PlaceStatus<'s> {
    #[must_use]
    pub fn parse(value: Value<'s, '_>) -> PlaceStatus<'s> {
        Self::parse_known(value).unwrap_or(PlaceStatus::Unrecognized)
    }

    fn parse_known(value: Value<'s, '_>) -> Option<PlaceStatus<'s>> {
        if let Some(resting) = value.get("resting") {
            return Some(PlaceStatus::Resting {
                oid: resting.get("oid")?.u64()?,
                cloid: parse_cloid(resting.get("cloid")),
            });
        }
        if let Some(filled) = value.get("filled") {
            return Some(PlaceStatus::Filled {
                total_sz: Sz::parse(filled.get("totalSz")?.text()?).ok()?,
                avg_px: Px::parse(filled.get("avgPx")?.text()?).ok()?,
                oid: filled.get("oid")?.u64()?,
                cloid: parse_cloid(filled.get("cloid")),
            });
        }
        Some(PlaceStatus::Error(value.get("error")?.str()?))
    }
}

/// One entry of an `/exchange` reply's `statuses`, for canceling an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelStatus<'s> {
    Success,
    /// The docs' `MissingOrder`, among others. It does not say why, so it decides nothing alone.
    Error(&'s str),
    Unrecognized,
}

impl<'s> CancelStatus<'s> {
    #[must_use]
    pub fn parse(value: Value<'s, '_>) -> CancelStatus<'s> {
        if value.str() == Some("success") {
            return CancelStatus::Success;
        }
        match value.get("error").and_then(Value::str) {
            Some(reason) => CancelStatus::Error(reason),
            None => CancelStatus::Unrecognized,
        }
    }
}

/// The top level of an `/exchange` reply.
#[derive(Clone, Copy, Debug)]
pub enum ExchangeReply<'s, 't> {
    /// The server processed the action. `statuses` lines up with the batch, when it has one.
    Ok { kind: &'s str, statuses: Option<Value<'s, 't>> },
    /// The whole action was refused, with no per-order detail: the docs' pre-validation path.
    Err(&'s str),
}

impl<'s, 't> ExchangeReply<'s, 't> {
    #[must_use]
    pub fn parse(root: Value<'s, 't>) -> Option<ExchangeReply<'s, 't>> {
        match root.get("status")?.str()? {
            "ok" => {
                let response = root.get("response")?;
                let statuses = response.get("data").and_then(|data| data.get("statuses"));
                Some(ExchangeReply::Ok { kind: response.get("type")?.str()?, statuses })
            }
            "err" => Some(ExchangeReply::Err(root.get("response")?.str()?)),
            _ => None,
        }
    }
}

/// A WebSocket post's reply.
#[derive(Clone, Copy, Debug)]
pub struct PostReply<'s, 't> {
    pub id: u64,
    pub kind: PostKind,
    pub payload: Value<'s, 't>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostKind {
    Info,
    Action,
    /// The server's words for what would have been an HTTP error status.
    Error,
}

impl<'s, 't> PostReply<'s, 't> {
    /// Reads the `data` of a `post` channel message.
    #[must_use]
    pub fn parse(data: Value<'s, 't>) -> Option<PostReply<'s, 't>> {
        let response = data.get("response")?;
        let kind = match response.get("type")?.str()? {
            "info" => PostKind::Info,
            "action" => PostKind::Action,
            "error" => PostKind::Error,
            _ => return None,
        };
        Some(PostReply { id: data.get("id")?.u64()?, kind, payload: response.get("payload")? })
    }
}

/// One perp market in `meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerpInfo {
    pub coin: Coin,
    pub sz_decimals: u8,
    pub leverage_max: u32,
    pub is_delisted: bool,
}

impl PerpInfo {
    #[must_use]
    pub fn parse(value: Value<'_, '_>) -> Option<PerpInfo> {
        Some(PerpInfo {
            coin: Coin::parse(value.get("name")?.text()?)?,
            sz_decimals: u8::try_from(value.get("szDecimals")?.u64()?).ok()?,
            leverage_max: u32::try_from(value.get("maxLeverage")?.u64()?).ok()?,
            is_delisted: value.get("isDelisted").and_then(Value::bool).unwrap_or(false),
        })
    }
}

/// A user's address on the wire, for a channel that names one.
#[must_use]
pub fn read_user(data: Value<'_, '_>) -> Option<Address> {
    Address::parse(data.get("user")?.text()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{Token, parse};
    use std::vec;

    fn with_fixture<R>(text: &str, read: impl FnOnce(Value<'_, '_>) -> R) -> R {
        let mut tokens = vec![Token::ZERO; 8192];
        let document = parse(text.as_bytes(), &mut tokens).unwrap();
        read(document.root())
    }

    #[test]
    fn status_words_classify_per_the_docs_table() {
        for word in ["open", "triggered"] {
            assert_eq!(StatusClass::parse(word), StatusClass::Live);
        }
        assert_eq!(StatusClass::parse("filled"), StatusClass::Filled);
        for word in [
            "canceled",
            "rejected",
            "marginCanceled",
            "vaultWithdrawalCanceled",
            "openInterestCapCanceled",
            "selfTradeCanceled",
            "reduceOnlyCanceled",
            "siblingFilledCanceled",
            "delistedCanceled",
            "liquidatedCanceled",
            "scheduledCancel",
            "tickRejected",
            "minTradeNtlRejected",
            "perpMarginRejected",
            "reduceOnlyRejected",
            "badAloPxRejected",
            "iocCancelRejected",
            "badTriggerPxRejected",
            "marketOrderNoLiquidityRejected",
            "positionIncreaseAtOpenInterestCapRejected",
            "positionFlipAtOpenInterestCapRejected",
            "tooAggressiveAtOpenInterestCapRejected",
            "openInterestIncreaseRejected",
            "insufficientSpotBalanceRejected",
            "oracleRejected",
            "perpMaxPositionRejected",
        ] {
            assert_eq!(StatusClass::parse(word), StatusClass::Dead, "{word}");
        }
        for word in ["", "weird", "Filled", "OPEN", "pendingSomething"] {
            assert_eq!(StatusClass::parse(word), StatusClass::Unknown, "{word}");
        }
    }

    #[test]
    fn reads_channels_and_unknown_ones_survive() {
        let root_text = r#"{"channel":"somethingNew","data":{"x":1}}"#;
        with_fixture(root_text, |root| {
            let message = Message::parse(root).unwrap();
            assert_eq!(message.channel, Channel::Other);
            assert_eq!(message.channel_name, "somethingNew");
            assert!(!message.is_snapshot());
        });
        with_fixture(r#"{"nope":1}"#, |root| assert!(Message::parse(root).is_none()));
        with_fixture(r#"{"channel":"user","data":{}}"#, |root| {
            assert_eq!(Message::parse(root).unwrap().channel, Channel::UserEvents);
        });
    }

    #[test]
    fn empty_bbo_sides_are_none() {
        with_fixture(r#"{"coin":"X","time":1,"bbo":[null,{"px":"1","sz":"2","n":3}]}"#, |data| {
            let bbo = Bbo::parse(data).unwrap();
            assert!(bbo.bid.is_none());
            assert_eq!(bbo.ask.unwrap().orders_count, 3);
        });
        with_fixture(r#"{"coin":"X","time":1,"bbo":[null]}"#, |data| {
            assert!(Bbo::parse(data).is_none());
        });
    }

    #[test]
    fn a_fill_survives_odd_accounting_but_not_odd_economics() {
        let good = r#"{"coin":"S","px":"1","sz":"2","side":"A","time":1,"oid":2,"tid":3,"fee":"not-a-number"}"#;
        with_fixture(good, |value| {
            let fill = Fill::parse(value).unwrap();
            assert_eq!(fill.fee, Decimal::ZERO);
            assert_eq!(fill.cloid, None);
        });
        let bad = r#"{"coin":"S","px":"x","sz":"2","side":"A","time":1,"oid":2,"tid":3}"#;
        with_fixture(bad, |value| assert!(Fill::parse(value).is_none()));
    }

    #[test]
    fn reply_tops_parse_and_unknowns_stay_unrecognized() {
        with_fixture(r#"{"status":"err","response":"Invalid nonce"}"#, |root| {
            assert!(matches!(
                ExchangeReply::parse(root),
                Some(ExchangeReply::Err("Invalid nonce"))
            ));
        });
        with_fixture(r#"{"status":"ok","response":{"type":"default"}}"#, |root| {
            let reply = ExchangeReply::parse(root).unwrap();
            assert!(matches!(reply, ExchangeReply::Ok { kind: "default", statuses: None }));
        });
        with_fixture(r#"{"status":"maybe"}"#, |root| assert!(ExchangeReply::parse(root).is_none()));
        with_fixture(r#"{"waitingForFill":{"oid":1}}"#, |value| {
            assert!(matches!(PlaceStatus::parse(value), PlaceStatus::Unrecognized));
        });
        with_fixture(r#"{"resting":{"oid":"not a number"}}"#, |value| {
            assert!(matches!(PlaceStatus::parse(value), PlaceStatus::Unrecognized));
        });
        with_fixture(r#"["success",{"error":"gone"},17]"#, |root| {
            let mut items = root.items();
            assert_eq!(CancelStatus::parse(items.next().unwrap()), CancelStatus::Success);
            assert_eq!(CancelStatus::parse(items.next().unwrap()), CancelStatus::Error("gone"));
            assert_eq!(CancelStatus::parse(items.next().unwrap()), CancelStatus::Unrecognized);
        });
    }
}
