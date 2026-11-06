//! Reads the shapes of traffic captured from mainnet on 2026-11-05. Account traffic is anonymized:
//! addresses, ids and hashes are replaced and its amounts and times altered, so these pin the form of
//! the messages and say nothing true about any account. Public market data is as it was sent.
//!
//! These pin what the server really sends, which is not always what the docs say: fills carry a
//! `cloid`, candles send strings where numbers are documented, and funding reaches 10 decimals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hypertiger::decimal::{Decimal, Px, Sz, Szi};
use hypertiger::feed::{
    AccountSummary, AssetContext, Bbo, Candle, Channel, Fill, Level, Message, OrderStatusReply,
    OrderUpdate, OrderView, Position, PostKind, PostReply, StatusClass, Trade, UserRateLimit,
};
use hypertiger::json::{Token, Value, parse};
use hypertiger::types::Side;

fn with<R>(text: &str, read: impl FnOnce(Value<'_, '_>) -> R) -> R {
    let mut tokens = vec![Token::ZERO; 8192];
    let document = parse(text.as_bytes(), &mut tokens).unwrap();
    read(document.root())
}

fn message_data<R>(text: &str, channel: Channel, read: impl FnOnce(Value<'_, '_>) -> R) -> R {
    with(text, |root| {
        let message = Message::parse(root).unwrap();
        assert_eq!(message.channel, channel);
        read(message.data.unwrap())
    })
}

#[test]
fn book_levels_are_sorted_and_exact() {
    message_data(include_str!("fixtures/l2Book.json"), Channel::L2Book, |data| {
        let mut sides = data.get("levels").unwrap().items();
        let bids = sides.next().unwrap();
        let asks = sides.next().unwrap();
        assert_eq!(bids.count(), 20);
        assert_eq!(asks.count(), 20);
        let best_bid = Level::parse(bids.items().next().unwrap()).unwrap();
        let best_ask = Level::parse(asks.items().next().unwrap()).unwrap();
        assert!(best_bid.px < best_ask.px);
        assert!(best_bid.orders_count > 0);
    });
}

#[test]
fn bbo_trades_and_candles_read() {
    message_data(include_str!("fixtures/bbo.json"), Channel::Bbo, |data| {
        let bbo = Bbo::parse(data).unwrap();
        assert_eq!(bbo.coin.as_str(), "BTC");
        assert!(bbo.bid.unwrap().px < bbo.ask.unwrap().px);
    });
    message_data(include_str!("fixtures/trades.json"), Channel::Trades, |data| {
        assert_eq!(data.count(), 3);
        for item in data.items() {
            let trade = Trade::parse(item).unwrap();
            assert!(trade.tid > 0);
            assert!(trade.sz > Sz::ZERO);
        }
    });
    message_data(include_str!("fixtures/candle.json"), Channel::Candle, |data| {
        let candle = Candle::parse(data).unwrap();
        assert!(candle.time_open_ms < candle.time_close_ms);
        assert!(candle.low <= candle.high);
    });
}

#[test]
fn asset_contexts_hold_ten_decimal_funding() {
    message_data(include_str!("fixtures/activeAssetCtx.json"), Channel::ActiveAssetCtx, |data| {
        let context = AssetContext::parse(data.get("ctx").unwrap());
        assert!(context.mark_px.is_some());
        assert!(context.oracle_px.is_some());
        assert_eq!(context.funding, Some(Decimal::parse(b"0.0000125").unwrap()));
        assert!(context.open_interest.is_some());
    });
    message_data(
        include_str!("fixtures/activeSpotAssetCtx.json"),
        Channel::ActiveSpotAssetCtx,
        |data| {
            let context = AssetContext::parse(data.get("ctx").unwrap());
            assert!(context.mark_px.is_some());
            assert!(context.oracle_px.is_none());
            assert!(context.funding.is_none());
        },
    );
}

#[test]
fn all_mids_every_entry_is_a_price() {
    message_data(include_str!("fixtures/allMids.json"), Channel::AllMids, |data| {
        let mids = data.get("mids").unwrap();
        assert!(mids.count() > 1000);
        let mut read = 0u32;
        for (coin, mid) in mids.entries() {
            assert!(coin.str().is_some());
            assert!(Px::parse(mid.text().unwrap()).is_ok(), "{:?}", coin.str());
            read += 1;
        }
        assert_eq!(read, mids.count());
    });
}

#[test]
fn control_messages_read() {
    with(include_str!("fixtures/pong.json"), |root| {
        assert_eq!(Message::parse(root).unwrap().channel, Channel::Pong);
    });
    with(include_str!("fixtures/subscriptionResponse_l2Book.json"), |root| {
        let message = Message::parse(root).unwrap();
        assert_eq!(message.channel, Channel::SubscriptionResponse);
    });
    message_data(include_str!("fixtures/post.json"), Channel::Post, |data| {
        let reply = PostReply::parse(data).unwrap();
        assert_eq!(reply.id, 7);
        assert_eq!(reply.kind, PostKind::Info);
    });
}

#[test]
fn a_fill_snapshot_is_flagged_and_its_fills_carry_cloids() {
    with(include_str!("fixtures/userFills_snapshot.json"), |root| {
        let message = Message::parse(root).unwrap();
        assert_eq!(message.channel, Channel::UserFills);
        assert!(message.is_snapshot());
        let fills = message.data.unwrap().get("fills").unwrap();
        assert!(fills.count() > 0);
        for item in fills.items() {
            let fill = Fill::parse(item).unwrap();
            assert!(fill.cloid.is_some(), "the docs omit this field, and the wire has it");
            assert!(fill.tid > 0);
        }
    });
}

#[test]
fn info_results_read() {
    with(include_str!("fixtures/info_userFills.json"), |root| {
        let fill = Fill::parse(root.items().next().unwrap()).unwrap();
        assert_eq!(fill.side, Side::Sell);
        assert!(fill.fee > Decimal::ZERO);
    });
    with(include_str!("fixtures/info_frontendOpenOrders.json"), |root| {
        for item in root.items() {
            let order = OrderView::parse(item).unwrap();
            assert!(order.oid > 0);
            assert_eq!(order.sz, order.sz_orig);
        }
    });
    with(include_str!("fixtures/info_clearinghouseState.json"), |root| {
        let summary = AccountSummary::parse(root).unwrap();
        assert!(summary.account_value > Decimal::ZERO);
        let positions = root.get("assetPositions").unwrap();
        let position = Position::parse(positions.items().next().unwrap()).unwrap();
        assert_eq!(position.coin.as_str(), "SOL");
        assert!(position.szi < Szi::ZERO);
        assert!(position.liquidation_px.is_some());
    });
    with(include_str!("fixtures/info_userRateLimit.json"), |root| {
        let limit = UserRateLimit::parse(root).unwrap();
        assert_eq!(limit.requests_cap, 10_000);
        assert!(limit.requests_used > limit.requests_cap);
    });
}

#[test]
fn order_status_reads_all_three_outcomes() {
    with(include_str!("fixtures/info_statusUnknown.json"), |root| {
        assert_eq!(OrderStatusReply::parse(root), Some(OrderStatusReply::Unknown));
    });
    let found = |text: &str| -> OrderUpdate {
        with(text, |root| match OrderStatusReply::parse(root) {
            Some(OrderStatusReply::Found(update)) => update,
            other => panic!("{other:?}"),
        })
    };
    let open = found(include_str!("fixtures/info_statusOpen.json"));
    assert_eq!(open.class, StatusClass::Live);
    assert_eq!(open.order.sz, open.order.sz_orig);

    // A filled order reports nothing remaining, while its original size still says what it was.
    let filled = found(include_str!("fixtures/info_statusFilled.json"));
    assert_eq!(filled.class, StatusClass::Filled);
    assert_eq!(filled.order.sz, Sz::ZERO);
    assert!(filled.order.sz_orig > Sz::ZERO);
    assert!(filled.order.cloid.is_some());
}
