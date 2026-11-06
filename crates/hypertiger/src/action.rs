//! Every action the exchange documents, as a single walk per action.
//!
//! Field order is part of the signature, since the exchange hashes msgpack in its own struct order,
//! so each walk below is the protocol. `docs/COVERAGE.md` records which of them are pinned against
//! the reference SDK and which against the live server.

use crate::decimal::{Px, Sz, Wire};
use crate::market::Asset;
use crate::signing::{
    ScalarHex, Signature, Signer, USER_CHAIN_ID_HEX, keccak, l1_action_hash, l1_digest,
    typed_digest, user_domain, word_address, word_u64,
};
use crate::sink::{Sink, Walk};
use crate::types::{Address, Cloid, Network, Side};
use sha3::{Digest, Keccak256};

/// The most orders or cancels this crate puts in one action. An action's IP weight is
/// `1 + floor(n / 40)`, so 39 is the most a single unit of weight buys.
pub const BATCH_SIZE_MAX: usize = 39;

const _: () = assert!((BATCH_SIZE_MAX as u32) / 40 == 0);

/// How long a resting order lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tif {
    /// Add liquidity only. Refused rather than matched.
    Alo,
    /// Immediate or cancel. The unfilled part dies instead of resting.
    Ioc,
    /// Good til canceled.
    Gtc,
}

impl Tif {
    const fn name(self) -> &'static str {
        match self {
            Tif::Alo => "Alo",
            Tif::Ioc => "Ioc",
            Tif::Gtc => "Gtc",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tpsl {
    Tp,
    Sl,
}

impl Tpsl {
    const fn name(self) -> &'static str {
        match self {
            Tpsl::Tp => "tp",
            Tpsl::Sl => "sl",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    Limit(Tif),
    Trigger { is_market: bool, trigger_px: Px, tpsl: Tpsl },
}

/// One order as it appears in an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderWire {
    pub asset: Asset,
    pub side: Side,
    pub px: Px,
    pub sz: Sz,
    pub reduce_only: bool,
    pub kind: OrderKind,
    pub cloid: Option<Cloid>,
}

impl Walk for OrderWire {
    fn walk<S: Sink>(&self, sink: &mut S) {
        sink.map(if self.cloid.is_some() { 7 } else { 6 });
        sink.key("a");
        sink.u64(u64::from(self.asset.raw()));
        sink.key("b");
        sink.bool(self.side.is_buy());
        sink.key("p");
        sink.str(self.px.to_wire().as_str());
        sink.key("s");
        sink.str(self.sz.to_wire().as_str());
        sink.key("r");
        sink.bool(self.reduce_only);
        sink.key("t");
        walk_order_kind(&self.kind, sink);
        if let Some(cloid) = self.cloid {
            sink.key("c");
            sink.str(cloid.to_hex().as_str());
        }
    }
}

fn walk_order_kind<S: Sink>(kind: &OrderKind, sink: &mut S) {
    sink.map(1);
    match kind {
        OrderKind::Limit(tif) => {
            sink.key("limit");
            sink.map(1);
            sink.key("tif");
            sink.str(tif.name());
        }
        OrderKind::Trigger { is_market, trigger_px, tpsl } => {
            sink.key("trigger");
            sink.map(3);
            sink.key("isMarket");
            sink.bool(*is_market);
            sink.key("triggerPx");
            sink.str(trigger_px.to_wire().as_str());
            sink.key("tpsl");
            sink.str(tpsl.name());
        }
    }
}

/// How the orders of one action relate to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    Na,
    NormalTpsl,
    PositionTpsl,
    /// A write-priority fee as a rate of `p / 100_000_000`, charged from staked HYPE.
    Priority(u64),
}

/// A builder and the fee it takes, in tenths of a basis point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Builder {
    pub address: Address,
    pub fee: u64,
}

/// An order to change, by exchange id or by the client's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderRef {
    Oid(u64),
    Cloid(Cloid),
}

impl OrderRef {
    fn walk<S: Sink>(self, sink: &mut S) {
        match self {
            OrderRef::Oid(oid) => sink.u64(oid),
            OrderRef::Cloid(cloid) => sink.str(cloid.to_hex().as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelWire {
    pub asset: Asset,
    pub oid: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelCloidWire {
    pub asset: Asset,
    pub cloid: Cloid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifyWire {
    pub target: OrderRef,
    pub order: OrderWire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TwapWire {
    pub asset: Asset,
    pub side: Side,
    pub sz: Sz,
    pub reduce_only: bool,
    pub minutes: u32,
    pub randomize: bool,
}

/// How far a trailing stop trails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retracement<'a> {
    /// A percent string, such as `"1.234%"`.
    Pct(&'a str),
    Px(Px),
}

/// The four ways to move value between outcome tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeAction {
    Split { outcome: u64, amount: Sz },
    Merge { outcome: u64, amount: Sz },
    MergeQuestion { question: u64, amount: Sz },
    Negate { question: u64, outcome: u64, amount: Sz },
}

/// Actions signed with the L1 scheme.
///
/// Deployer and validator actions are not typed here; they are few, rarely used and large.
/// Implement
/// `Walk` for one and it signs through `sign_l1` like any other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action<'a> {
    Order {
        orders: &'a [OrderWire],
        grouping: Grouping,
        builder: Option<Builder>,
    },
    /// `fast` skips trigger-order lookups. The docs: "no disadvantage", except that a fast cancel
    /// cannot cancel a trigger order. It is a field and not a default so that the choice is seen.
    Cancel {
        cancels: &'a [CancelWire],
        fast: bool,
    },
    CancelByCloid {
        cancels: &'a [CancelCloidWire],
        fast: bool,
    },
    /// The dead man's switch. `None` unsets it.
    ScheduleCancel {
        time_ms: Option<u64>,
    },
    Modify {
        target: OrderRef,
        order: OrderWire,
        always_place: bool,
    },
    BatchModify {
        modifies: &'a [ModifyWire],
    },
    UpdateLeverage {
        asset: Asset,
        is_cross: bool,
        leverage: u32,
    },
    /// `ntli` is USD with 6 decimals, signed: negative removes margin.
    UpdateIsolatedMargin {
        asset: Asset,
        ntli: i64,
    },
    TwapOrder {
        twap: TwapWire,
    },
    TwapCancel {
        asset: Asset,
        twap_id: u64,
    },
    TrailingStop {
        asset: Asset,
        side: Side,
        sz: Sz,
        reduce_only: bool,
        retracement: Retracement<'a>,
        activation_px: Option<Px>,
    },
    VaultTransfer {
        vault: Address,
        is_deposit: bool,
        usd: u64,
    },
    SubAccountTransfer {
        sub_account: Address,
        is_deposit: bool,
        usd: u64,
    },
    SubAccountSpotTransfer {
        sub_account: Address,
        is_deposit: bool,
        token: &'a str,
        amount: Sz,
    },
    Hip3LiquidatorTransfer {
        dex: &'a str,
        ntl: u64,
        is_deposit: bool,
    },
    CreateSubAccount {
        name: &'a str,
    },
    SetReferrer {
        code: &'a str,
    },
    EvmUserModify {
        using_big_blocks: bool,
    },
    /// Burns a nonce, which fences off any pending action signed with the same one.
    Noop,
    ClaimRewards,
    ReserveRequestWeight {
        weight: u64,
        destination: Option<Address>,
    },
    GossipPriorityBid {
        slot_id: u64,
        ip: &'a str,
        max_gas: u64,
    },
    CDeposit2 {
        wei: u64,
    },
    AgentSendAsset {
        destination: Address,
        source_dex: &'a str,
        destination_dex: &'a str,
        token: &'a str,
        amount: Sz,
        from_sub_account: Option<Address>,
        nonce_ms: u64,
    },
    AgentEnableDexAbstraction,
    AgentSetAbstraction {
        abstraction: &'a str,
    },
    UserOutcome(OutcomeAction),
    /// Any action whose only field besides its tag is `type`. For new argument-free actions.
    Bare {
        kind: &'a str,
    },
}

impl Walk for Action<'_> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        match *self {
            Action::Order { orders, grouping, builder } => {
                walk_order(orders, grouping, builder, sink);
            }
            Action::Cancel { cancels, fast } => walk_cancel(cancels, fast, sink),
            Action::CancelByCloid { cancels, fast } => walk_cancel_cloid(cancels, fast, sink),
            Action::ScheduleCancel { time_ms } => walk_schedule_cancel(time_ms, sink),
            Action::Modify { target, order, always_place } => {
                walk_modify(target, &order, always_place, sink);
            }
            Action::BatchModify { modifies } => walk_batch_modify(modifies, sink),
            Action::TwapOrder { twap } => walk_twap(&twap, sink),
            Action::TrailingStop { asset, side, sz, reduce_only, retracement, activation_px } => {
                walk_trailing(asset, side, sz, reduce_only, retracement, activation_px, sink);
            }
            Action::UserOutcome(outcome) => walk_outcome(outcome, sink),
            Action::AgentSendAsset { .. } => walk_agent_send_asset(self, sink),
            _ => walk_simple(self, sink),
        }
    }
}

fn walk_order<S: Sink>(
    orders: &[OrderWire],
    grouping: Grouping,
    builder: Option<Builder>,
    sink: &mut S,
) {
    assert_ne!(orders, []);
    assert!(u32::try_from(orders.len()).is_ok());
    sink.map(if builder.is_some() { 4 } else { 3 });
    sink.key("type");
    sink.str("order");
    sink.key("orders");
    sink.array(orders.len() as u32);
    for order in orders {
        order.walk(sink);
    }
    sink.key("grouping");
    match grouping {
        Grouping::Na => sink.str("na"),
        Grouping::NormalTpsl => sink.str("normalTpsl"),
        Grouping::PositionTpsl => sink.str("positionTpsl"),
        Grouping::Priority(rate) => {
            sink.map(1);
            sink.key("p");
            sink.u64(rate);
        }
    }
    if let Some(builder) = builder {
        sink.key("builder");
        sink.map(2);
        sink.key("b");
        sink.str(builder.address.to_hex().as_str());
        sink.key("f");
        sink.u64(builder.fee);
    }
}

fn walk_cancel<S: Sink>(cancels: &[CancelWire], fast: bool, sink: &mut S) {
    assert_ne!(cancels, []);
    assert!(u32::try_from(cancels.len()).is_ok());
    sink.map(if fast { 3 } else { 2 });
    sink.key("type");
    sink.str("cancel");
    sink.key("cancels");
    sink.array(cancels.len() as u32);
    for cancel in cancels {
        sink.map(2);
        sink.key("a");
        sink.u64(u64::from(cancel.asset.raw()));
        sink.key("o");
        sink.u64(cancel.oid);
    }
    if fast {
        sink.key("f");
        sink.bool(true);
    }
}

fn walk_cancel_cloid<S: Sink>(cancels: &[CancelCloidWire], fast: bool, sink: &mut S) {
    assert_ne!(cancels, []);
    assert!(u32::try_from(cancels.len()).is_ok());
    sink.map(if fast { 3 } else { 2 });
    sink.key("type");
    sink.str("cancelByCloid");
    sink.key("cancels");
    sink.array(cancels.len() as u32);
    for cancel in cancels {
        sink.map(2);
        sink.key("asset");
        sink.u64(u64::from(cancel.asset.raw()));
        sink.key("cloid");
        sink.str(cancel.cloid.to_hex().as_str());
    }
    if fast {
        sink.key("f");
        sink.bool(true);
    }
}

fn walk_schedule_cancel<S: Sink>(time_ms: Option<u64>, sink: &mut S) {
    sink.map(if time_ms.is_some() { 2 } else { 1 });
    sink.key("type");
    sink.str("scheduleCancel");
    if let Some(time_ms) = time_ms {
        sink.key("time");
        sink.u64(time_ms);
    }
}

fn walk_modify<S: Sink>(target: OrderRef, order: &OrderWire, always_place: bool, sink: &mut S) {
    sink.map(if always_place { 4 } else { 3 });
    sink.key("type");
    sink.str("modify");
    sink.key("oid");
    target.walk(sink);
    sink.key("order");
    order.walk(sink);
    if always_place {
        sink.key("a");
        sink.bool(true);
    }
}

fn walk_batch_modify<S: Sink>(modifies: &[ModifyWire], sink: &mut S) {
    assert_ne!(modifies, []);
    assert!(u32::try_from(modifies.len()).is_ok());
    sink.map(2);
    sink.key("type");
    sink.str("batchModify");
    sink.key("modifies");
    sink.array(modifies.len() as u32);
    for modify in modifies {
        sink.map(2);
        sink.key("oid");
        modify.target.walk(sink);
        sink.key("order");
        modify.order.walk(sink);
    }
}

fn walk_twap<S: Sink>(twap: &TwapWire, sink: &mut S) {
    assert!(twap.minutes > 0);
    sink.map(2);
    sink.key("type");
    sink.str("twapOrder");
    sink.key("twap");
    sink.map(6);
    sink.key("a");
    sink.u64(u64::from(twap.asset.raw()));
    sink.key("b");
    sink.bool(twap.side.is_buy());
    sink.key("s");
    sink.str(twap.sz.to_wire().as_str());
    sink.key("r");
    sink.bool(twap.reduce_only);
    sink.key("m");
    sink.u64(u64::from(twap.minutes));
    sink.key("t");
    sink.bool(twap.randomize);
}

fn walk_trailing<S: Sink>(
    asset: Asset,
    side: Side,
    sz: Sz,
    reduce_only: bool,
    retracement: Retracement<'_>,
    activation_px: Option<Px>,
    sink: &mut S,
) {
    sink.map(7);
    sink.key("type");
    sink.str("trailingStop");
    sink.key("asset");
    sink.u64(u64::from(asset.raw()));
    sink.key("isBuy");
    sink.bool(side.is_buy());
    sink.key("sz");
    sink.str(sz.to_wire().as_str());
    sink.key("reduceOnly");
    sink.bool(reduce_only);
    sink.key("retracement");
    sink.map(1);
    match retracement {
        Retracement::Pct(pct) => {
            sink.key("pct");
            sink.str(pct);
        }
        Retracement::Px(px) => {
            sink.key("px");
            sink.str(px.to_wire().as_str());
        }
    }
    sink.key("activationPx");
    match activation_px {
        Some(px) => sink.str(px.to_wire().as_str()),
        None => sink.nil(),
    }
}

fn walk_outcome<S: Sink>(outcome: OutcomeAction, sink: &mut S) {
    sink.map(2);
    sink.key("type");
    sink.str("userOutcome");
    match outcome {
        OutcomeAction::Split { outcome, amount } => {
            walk_outcome_pair("splitOutcome", "outcome", outcome, amount, sink);
        }
        OutcomeAction::Merge { outcome, amount } => {
            walk_outcome_pair("mergeOutcome", "outcome", outcome, amount, sink);
        }
        OutcomeAction::MergeQuestion { question, amount } => {
            walk_outcome_pair("mergeQuestion", "question", question, amount, sink);
        }
        OutcomeAction::Negate { question, outcome, amount } => {
            sink.key("negateOutcome");
            sink.map(3);
            sink.key("question");
            sink.u64(question);
            sink.key("outcome");
            sink.u64(outcome);
            sink.key("amount");
            sink.str(amount.to_wire().as_str());
        }
    }
}

fn walk_outcome_pair<S: Sink>(name: &str, field: &str, id: u64, amount: Sz, sink: &mut S) {
    sink.key(name);
    sink.map(2);
    sink.key(field);
    sink.u64(id);
    sink.key("amount");
    sink.str(amount.to_wire().as_str());
}

fn walk_agent_send_asset<S: Sink>(action: &Action<'_>, sink: &mut S) {
    let Action::AgentSendAsset {
        destination,
        source_dex,
        destination_dex,
        token,
        amount,
        from_sub_account,
        nonce_ms,
    } = *action
    else {
        panic!("only AgentSendAsset is dispatched here");
    };
    sink.map(8);
    sink.key("type");
    sink.str("agentSendAsset");
    sink.key("destination");
    sink.str(destination.to_hex().as_str());
    sink.key("sourceDex");
    sink.str(source_dex);
    sink.key("destinationDex");
    sink.str(destination_dex);
    sink.key("token");
    sink.str(token);
    sink.key("amount");
    sink.str(amount.to_wire().as_str());
    sink.key("fromSubAccount");
    match from_sub_account {
        Some(address) => sink.str(address.to_hex().as_str()),
        None => sink.str(""),
    }
    sink.key("nonce");
    sink.u64(nonce_ms);
}

/// The flat actions: a tag and a few scalars, walked from one table each.
fn walk_simple<S: Sink>(action: &Action<'_>, sink: &mut S) {
    match *action {
        Action::UpdateLeverage { asset, is_cross, leverage } => {
            assert!(leverage > 0);
            sink.map(4);
            entry_str(sink, "type", "updateLeverage");
            entry_u64(sink, "asset", u64::from(asset.raw()));
            entry_bool(sink, "isCross", is_cross);
            entry_u64(sink, "leverage", u64::from(leverage));
        }
        Action::UpdateIsolatedMargin { asset, ntli } => {
            sink.map(4);
            entry_str(sink, "type", "updateIsolatedMargin");
            entry_u64(sink, "asset", u64::from(asset.raw()));
            // The docs: "won't have any effect until hedge mode is introduced".
            entry_bool(sink, "isBuy", true);
            sink.key("ntli");
            sink.i64(ntli);
        }
        Action::TwapCancel { asset, twap_id } => {
            sink.map(3);
            entry_str(sink, "type", "twapCancel");
            entry_u64(sink, "a", u64::from(asset.raw()));
            entry_u64(sink, "t", twap_id);
        }
        Action::VaultTransfer { vault, is_deposit, usd } => {
            sink.map(4);
            entry_str(sink, "type", "vaultTransfer");
            entry_str(sink, "vaultAddress", vault.to_hex().as_str());
            entry_bool(sink, "isDeposit", is_deposit);
            entry_u64(sink, "usd", usd);
        }
        Action::SubAccountTransfer { sub_account, is_deposit, usd } => {
            sink.map(4);
            entry_str(sink, "type", "subAccountTransfer");
            entry_str(sink, "subAccountUser", sub_account.to_hex().as_str());
            entry_bool(sink, "isDeposit", is_deposit);
            entry_u64(sink, "usd", usd);
        }
        Action::SubAccountSpotTransfer { sub_account, is_deposit, token, amount } => {
            sink.map(5);
            entry_str(sink, "type", "subAccountSpotTransfer");
            entry_str(sink, "subAccountUser", sub_account.to_hex().as_str());
            entry_bool(sink, "isDeposit", is_deposit);
            entry_str(sink, "token", token);
            entry_str(sink, "amount", amount.to_wire().as_str());
        }
        _ => walk_simple_rest(action, sink),
    }
}

fn walk_simple_rest<S: Sink>(action: &Action<'_>, sink: &mut S) {
    match *action {
        Action::Hip3LiquidatorTransfer { dex, ntl, is_deposit } => {
            sink.map(4);
            entry_str(sink, "type", "hip3LiquidatorTransfer");
            entry_str(sink, "dex", dex);
            entry_u64(sink, "ntl", ntl);
            entry_bool(sink, "isDeposit", is_deposit);
        }
        Action::CreateSubAccount { name } => tag_and_str(sink, "createSubAccount", "name", name),
        Action::SetReferrer { code } => tag_and_str(sink, "setReferrer", "code", code),
        Action::EvmUserModify { using_big_blocks } => {
            sink.map(2);
            entry_str(sink, "type", "evmUserModify");
            entry_bool(sink, "usingBigBlocks", using_big_blocks);
        }
        Action::ReserveRequestWeight { weight, destination } => {
            // The docs: "This field is skipped in hashing if it is unset."
            sink.map(if destination.is_some() { 3 } else { 2 });
            entry_str(sink, "type", "reserveRequestWeight");
            entry_u64(sink, "weight", weight);
            if let Some(address) = destination {
                entry_str(sink, "destination", address.to_hex().as_str());
            }
        }
        Action::GossipPriorityBid { slot_id, ip, max_gas } => {
            sink.map(4);
            entry_str(sink, "type", "gossipPriorityBid");
            entry_u64(sink, "slotId", slot_id);
            entry_str(sink, "ip", ip);
            entry_u64(sink, "maxGas", max_gas);
        }
        Action::CDeposit2 { wei } => {
            sink.map(2);
            entry_str(sink, "type", "cDeposit2");
            entry_u64(sink, "wei", wei);
        }
        Action::AgentSetAbstraction { abstraction } => {
            tag_and_str(sink, "agentSetAbstraction", "abstraction", abstraction);
        }
        Action::Noop => bare(sink, "noop"),
        Action::ClaimRewards => bare(sink, "claimRewards"),
        Action::AgentEnableDexAbstraction => bare(sink, "agentEnableDexAbstraction"),
        Action::Bare { kind } => bare(sink, kind),
        _ => panic!("every other action is dispatched by Action::walk or walk_simple"),
    }
}

fn entry_str<S: Sink>(sink: &mut S, key: &str, value: &str) {
    sink.key(key);
    sink.str(value);
}

fn entry_u64<S: Sink>(sink: &mut S, key: &str, value: u64) {
    sink.key(key);
    sink.u64(value);
}

fn entry_bool<S: Sink>(sink: &mut S, key: &str, value: bool) {
    sink.key(key);
    sink.bool(value);
}

fn tag_and_str<S: Sink>(sink: &mut S, kind: &str, key: &str, value: &str) {
    assert_ne!(kind, "");
    sink.map(2);
    entry_str(sink, "type", kind);
    entry_str(sink, key, value);
}

fn bare<S: Sink>(sink: &mut S, kind: &str) {
    assert_ne!(kind, "");
    sink.map(1);
    entry_str(sink, "type", kind);
}

/// Who an L1 action is signed for, and how long it may land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L1Options {
    pub network: Network,
    pub nonce: u64,
    /// The vault or sub-account traded for. `None` trades the signer's own master account.
    pub vault: Option<Address>,
    /// After this time the exchange refuses the action. See `docs/DESIGN.md`: this is what makes an
    /// ambiguous outcome decidable. An action that lands stale costs 5x the usual rate limit.
    pub expires_after_ms: Option<u64>,
}

/// Signs an L1 action.
pub fn sign_l1<W: Walk, A: Signer>(action: &W, options: &L1Options, signer: &A) -> Signature {
    assert!(options.nonce > 0, "the exchange refuses a zero nonce outside of test vectors");
    let hash = l1_action_hash(action, options.nonce, options.vault, options.expires_after_ms);
    signer.sign_digest(&l1_digest(&hash, options.network))
}

/// The full `/exchange` request body, or the WebSocket post's `payload`.
#[derive(Clone, Copy, Debug)]
pub struct Envelope<'a, W: Walk> {
    pub action: &'a W,
    pub nonce: u64,
    pub signature: Signature,
    pub vault: Option<Address>,
    pub expires_after_ms: Option<u64>,
}

impl<W: Walk> Walk for Envelope<'_, W> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        assert!(self.signature.v == 27 || self.signature.v == 28);
        sink.map(5);
        sink.key("action");
        self.action.walk(sink);
        entry_u64(sink, "nonce", self.nonce);
        sink.key("signature");
        walk_signature(&self.signature, sink);
        sink.key("vaultAddress");
        match self.vault {
            Some(address) => sink.str(address.to_hex().as_str()),
            None => sink.nil(),
        }
        sink.key("expiresAfter");
        match self.expires_after_ms {
            Some(expires_after_ms) => sink.u64(expires_after_ms),
            None => sink.nil(),
        }
    }
}

/// `{"r":..,"s":..,"v":..}`, the form both the envelope and multi-sig's inner list use.
pub fn walk_signature<S: Sink>(signature: &Signature, sink: &mut S) {
    sink.map(3);
    entry_str(sink, "r", ScalarHex::new(&signature.r).as_str());
    entry_str(sink, "s", ScalarHex::new(&signature.s).as_str());
    entry_u64(sink, "v", u64::from(signature.v));
}

/// The most fields any user-signed action carries, including `hyperliquidChain` and the two
/// multi-sig fields.
pub const USER_FIELDS_MAX: usize = 12;

/// A field's value, with its EIP-712 type implied by the variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldValue<'a> {
    Str(&'a str),
    /// A canonical number, signed as a `string`.
    Number(Wire),
    /// An address signed as a `string`, which is why its case matters to the exchange.
    AddressStr(Address),
    Address(Address),
    U64(u64),
    U32(u32),
    Bool(bool),
    Bytes32([u8; 32]),
    /// Arbitrary bytes, signed by hash and sent as `0x` hex.
    Bytes(&'a [u8]),
}

impl FieldValue<'_> {
    const fn type_name(&self) -> &'static str {
        match self {
            FieldValue::Str(_) | FieldValue::Number(_) | FieldValue::AddressStr(_) => "string",
            FieldValue::Address(_) => "address",
            FieldValue::U64(_) => "uint64",
            FieldValue::U32(_) => "uint32",
            FieldValue::Bool(_) => "bool",
            FieldValue::Bytes32(_) => "bytes32",
            FieldValue::Bytes(_) => "bytes",
        }
    }

    fn word(&self) -> [u8; 32] {
        match self {
            FieldValue::Str(text) => keccak(text.as_bytes()),
            FieldValue::Number(wire) => keccak(wire.as_bytes()),
            FieldValue::AddressStr(address) => keccak(address.to_hex().as_str().as_bytes()),
            FieldValue::Address(address) => word_address(*address),
            FieldValue::U64(value) => word_u64(*value),
            FieldValue::U32(value) => word_u64(u64::from(*value)),
            FieldValue::Bool(value) => word_u64(u64::from(*value)),
            FieldValue::Bytes32(bytes) => *bytes,
            FieldValue::Bytes(bytes) => keccak(bytes),
        }
    }

    fn walk<S: Sink>(&self, sink: &mut S) {
        match self {
            FieldValue::Str(text) => sink.str(text),
            FieldValue::Number(wire) => sink.str(wire.as_str()),
            FieldValue::AddressStr(address) | FieldValue::Address(address) => {
                sink.str(address.to_hex().as_str());
            }
            FieldValue::U64(value) => sink.u64(*value),
            FieldValue::U32(value) => sink.u64(u64::from(*value)),
            FieldValue::Bool(value) => sink.bool(*value),
            FieldValue::Bytes32(bytes) => walk_hex(bytes, sink),
            FieldValue::Bytes(bytes) => walk_hex(bytes, sink),
        }
    }
}

/// The most bytes a `data` field carries. The EVM call data this is for is short, and a stack
/// buffer of twice this size is what renders it.
pub const USER_DATA_SIZE_MAX: usize = 1024;

fn walk_hex<S: Sink>(bytes: &[u8], sink: &mut S) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    assert!(bytes.len() <= USER_DATA_SIZE_MAX);
    let mut text = [0u8; 2 + 2 * USER_DATA_SIZE_MAX];
    text[0] = b'0';
    text[1] = b'x';
    for (index, byte) in bytes.iter().enumerate() {
        text[2 + 2 * index] = HEX[usize::from(byte >> 4)];
        text[3 + 2 * index] = HEX[usize::from(byte & 0x0f)];
    }
    let Ok(rendered) = core::str::from_utf8(&text[..2 + 2 * bytes.len()]) else {
        panic!("hex output is ASCII");
    };
    sink.str(rendered);
}

/// One named field of a user-signed action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field<'a> {
    pub name: &'static str,
    pub value: FieldValue<'a>,
}

impl Field<'_> {
    /// An unnamed agent signs `agentName` as empty and sends no such field: the reference does
    /// exactly this, so the two must differ here.
    fn omitted_from_body(&self) -> bool {
        self.name == "agentName" && matches!(self.value, FieldValue::Str(""))
    }
}

const fn field<'a>(name: &'static str, value: FieldValue<'a>) -> Field<'a> {
    Field { name, value }
}

/// The arguments of `UserAction::send_asset`, named because three of them are dex and token strings
/// that a positional call could swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetTransfer<'a> {
    pub destination: Address,
    /// The perp dex to take from. Empty for the default dex, `spot` for spot.
    pub source_dex: &'a str,
    pub destination_dex: &'a str,
    pub token: &'a str,
    pub amount: Sz,
    pub from_sub_account: Option<Address>,
}

/// The arguments of `UserAction::send_to_evm_with_data`, named because several are strings that a
/// positional call could swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvmTransfer<'a> {
    pub token: &'a str,
    pub amount: Sz,
    pub source_dex: &'a str,
    pub destination_recipient: &'a str,
    /// `hex` or `base58`.
    pub address_encoding: &'a str,
    pub destination_chain_id: u32,
    pub gas_limit: u64,
    pub data: &'a [u8],
}

/// An action signed with the user scheme: EIP-712 over named fields, so a wallet can show it.
///
/// Each constructor is the reference's signing table for that action, and the field order is the
/// EIP-712 type string. All of them take the nonce once, because the exchange requires the `time`
/// or `nonce` inside the action to equal the one outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserAction<'a> {
    action_type: &'static str,
    primary_type: &'static str,
    network: Network,
    fields: [Option<Field<'a>>; USER_FIELDS_MAX],
    nonce_ms: u64,
}

impl<'a> UserAction<'a> {
    fn new(
        action_type: &'static str,
        primary_type: &'static str,
        network: Network,
        nonce_ms: u64,
        fields: &[Field<'a>],
    ) -> UserAction<'a> {
        assert!(primary_type.starts_with("HyperliquidTransaction:"));
        assert!(fields.len() < USER_FIELDS_MAX - 2);
        let mut stored = [None; USER_FIELDS_MAX];
        stored[0] = Some(field("hyperliquidChain", FieldValue::Str(network.chain_name())));
        for (index, item) in fields.iter().enumerate() {
            stored[index + 1] = Some(*item);
        }
        UserAction { action_type, primary_type, network, fields: stored, nonce_ms }
    }

    #[must_use]
    pub const fn nonce_ms(&self) -> u64 {
        self.nonce_ms
    }

    #[must_use]
    pub const fn network(&self) -> Network {
        self.network
    }

    #[must_use]
    pub fn usd_send(network: Network, destination: Address, amount: Sz, time_ms: u64) -> Self {
        Self::new(
            "usdSend",
            "HyperliquidTransaction:UsdSend",
            network,
            time_ms,
            &[
                field("destination", FieldValue::AddressStr(destination)),
                field("amount", FieldValue::Number(amount.to_wire())),
                field("time", FieldValue::U64(time_ms)),
            ],
        )
    }

    #[must_use]
    pub fn spot_send(
        network: Network,
        destination: Address,
        token: &'a str,
        amount: Sz,
        time_ms: u64,
    ) -> Self {
        Self::new(
            "spotSend",
            "HyperliquidTransaction:SpotSend",
            network,
            time_ms,
            &[
                field("destination", FieldValue::AddressStr(destination)),
                field("token", FieldValue::Str(token)),
                field("amount", FieldValue::Number(amount.to_wire())),
                field("time", FieldValue::U64(time_ms)),
            ],
        )
    }

    #[must_use]
    pub fn withdraw(network: Network, destination: Address, amount: Sz, time_ms: u64) -> Self {
        Self::new(
            "withdraw3",
            "HyperliquidTransaction:Withdraw",
            network,
            time_ms,
            &[
                field("destination", FieldValue::AddressStr(destination)),
                field("amount", FieldValue::Number(amount.to_wire())),
                field("time", FieldValue::U64(time_ms)),
            ],
        )
    }

    /// `amount` is a string because a sub-account transfer appends ` subaccount:0x..` to the
    /// number.
    #[must_use]
    pub fn usd_class_transfer(
        network: Network,
        amount: &'a str,
        to_perp: bool,
        nonce_ms: u64,
    ) -> Self {
        Self::new(
            "usdClassTransfer",
            "HyperliquidTransaction:UsdClassTransfer",
            network,
            nonce_ms,
            &[
                field("amount", FieldValue::Str(amount)),
                field("toPerp", FieldValue::Bool(to_perp)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    #[must_use]
    pub fn send_asset(network: Network, transfer: &AssetTransfer<'a>, nonce_ms: u64) -> Self {
        let from = match transfer.from_sub_account {
            Some(address) => FieldValue::AddressStr(address),
            None => FieldValue::Str(""),
        };
        Self::new(
            "sendAsset",
            "HyperliquidTransaction:SendAsset",
            network,
            nonce_ms,
            &[
                field("destination", FieldValue::AddressStr(transfer.destination)),
                field("sourceDex", FieldValue::Str(transfer.source_dex)),
                field("destinationDex", FieldValue::Str(transfer.destination_dex)),
                field("token", FieldValue::Str(transfer.token)),
                field("amount", FieldValue::Number(transfer.amount.to_wire())),
                field("fromSubAccount", from),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    #[must_use]
    pub fn c_deposit(network: Network, wei: u64, nonce_ms: u64) -> Self {
        Self::new(
            "cDeposit",
            "HyperliquidTransaction:CDeposit",
            network,
            nonce_ms,
            &[field("wei", FieldValue::U64(wei)), field("nonce", FieldValue::U64(nonce_ms))],
        )
    }

    #[must_use]
    pub fn c_withdraw(network: Network, wei: u64, nonce_ms: u64) -> Self {
        Self::new(
            "cWithdraw",
            "HyperliquidTransaction:CWithdraw",
            network,
            nonce_ms,
            &[field("wei", FieldValue::U64(wei)), field("nonce", FieldValue::U64(nonce_ms))],
        )
    }

    #[must_use]
    pub fn token_delegate(
        network: Network,
        validator: Address,
        wei: u64,
        is_undelegate: bool,
        nonce_ms: u64,
    ) -> Self {
        Self::new(
            "tokenDelegate",
            "HyperliquidTransaction:TokenDelegate",
            network,
            nonce_ms,
            &[
                field("validator", FieldValue::Address(validator)),
                field("wei", FieldValue::U64(wei)),
                field("isUndelegate", FieldValue::Bool(is_undelegate)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// `name` empty registers the unnamed API wallet, replacing any existing one.
    #[must_use]
    pub fn approve_agent(network: Network, agent: Address, name: &'a str, nonce_ms: u64) -> Self {
        Self::new(
            "approveAgent",
            "HyperliquidTransaction:ApproveAgent",
            network,
            nonce_ms,
            &[
                field("agentAddress", FieldValue::Address(agent)),
                field("agentName", FieldValue::Str(name)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// `max_fee_rate` is a percent string, such as `"0.001%"`.
    #[must_use]
    pub fn approve_builder_fee(
        network: Network,
        max_fee_rate: &'a str,
        builder: Address,
        nonce_ms: u64,
    ) -> Self {
        Self::new(
            "approveBuilderFee",
            "HyperliquidTransaction:ApproveBuilderFee",
            network,
            nonce_ms,
            &[
                field("maxFeeRate", FieldValue::Str(max_fee_rate)),
                field("builder", FieldValue::Address(builder)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// `signers` is the JSON text the exchange expects, with `authorizedUsers` already sorted.
    #[must_use]
    pub fn convert_to_multi_sig(network: Network, signers: &'a str, nonce_ms: u64) -> Self {
        Self::new(
            "convertToMultiSigUser",
            "HyperliquidTransaction:ConvertToMultiSigUser",
            network,
            nonce_ms,
            &[
                field("signers", FieldValue::Str(signers)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    #[must_use]
    pub fn user_dex_abstraction(
        network: Network,
        user: Address,
        enabled: bool,
        nonce_ms: u64,
    ) -> Self {
        Self::new(
            "userDexAbstraction",
            "HyperliquidTransaction:UserDexAbstraction",
            network,
            nonce_ms,
            &[
                field("user", FieldValue::Address(user)),
                field("enabled", FieldValue::Bool(enabled)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// `abstraction` is `disabled`, `unifiedAccount` or `portfolioMargin`.
    #[must_use]
    pub fn user_set_abstraction(
        network: Network,
        user: Address,
        abstraction: &'a str,
        nonce_ms: u64,
    ) -> Self {
        Self::new(
            "userSetAbstraction",
            "HyperliquidTransaction:UserSetAbstraction",
            network,
            nonce_ms,
            &[
                field("user", FieldValue::Address(user)),
                field("abstraction", FieldValue::Str(abstraction)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// A Core to EVM transfer that calls `coreReceiveWithData` on the linked contract.
    /// `data` is at most `USER_DATA_SIZE_MAX` bytes.
    #[must_use]
    pub fn send_to_evm_with_data(
        network: Network,
        transfer: &EvmTransfer<'a>,
        nonce_ms: u64,
    ) -> Self {
        assert!(transfer.data.len() <= USER_DATA_SIZE_MAX);
        Self::new(
            "sendToEvmWithData",
            "HyperliquidTransaction:SendToEvmWithData",
            network,
            nonce_ms,
            &[
                field("token", FieldValue::Str(transfer.token)),
                field("amount", FieldValue::Number(transfer.amount.to_wire())),
                field("sourceDex", FieldValue::Str(transfer.source_dex)),
                field("destinationRecipient", FieldValue::Str(transfer.destination_recipient)),
                field("addressEncoding", FieldValue::Str(transfer.address_encoding)),
                field("destinationChainId", FieldValue::U32(transfer.destination_chain_id)),
                field("gasLimit", FieldValue::U64(transfer.gas_limit)),
                field("data", FieldValue::Bytes(transfer.data)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// The multi-sig envelope every signer of a `multiSig` action signs.
    #[must_use]
    pub fn multi_sig_envelope(network: Network, action_hash: [u8; 32], nonce_ms: u64) -> Self {
        Self::new(
            "multiSigEnvelope",
            "HyperliquidTransaction:SendMultiSig",
            network,
            nonce_ms,
            &[
                field("multiSigActionHash", FieldValue::Bytes32(action_hash)),
                field("nonce", FieldValue::U64(nonce_ms)),
            ],
        )
    }

    /// Adds the two fields a multi-sig payload signature binds, right after `hyperliquidChain`.
    #[must_use]
    pub fn for_multi_sig(self, multi_sig_user: Address, outer_signer: Address) -> Self {
        let mut stored = [None; USER_FIELDS_MAX];
        stored[0] = self.fields[0];
        assert!(stored[0].is_some());
        stored[1] = Some(field("payloadMultiSigUser", FieldValue::Address(multi_sig_user)));
        stored[2] = Some(field("outerSigner", FieldValue::Address(outer_signer)));
        stored[3..USER_FIELDS_MAX].copy_from_slice(&self.fields[1..USER_FIELDS_MAX - 2]);
        assert!(self.fields[USER_FIELDS_MAX - 1].is_none());
        assert!(self.fields[USER_FIELDS_MAX - 2].is_none());
        UserAction { fields: stored, ..self }
    }

    fn fields(&self) -> impl Iterator<Item = &Field<'a>> {
        self.fields.iter().map_while(Option::as_ref)
    }

    /// The digest the signer signs.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Keccak256::new();
        hasher.update(self.type_hash());
        for item in self.fields() {
            hasher.update(item.value.word());
        }
        typed_digest(&user_domain(), &hasher.finalize().into())
    }

    /// `keccak("Primary(type name,type name,...)")`, streamed so that it needs no buffer.
    fn type_hash(&self) -> [u8; 32] {
        let mut hasher = Keccak256::new();
        hasher.update(self.primary_type.as_bytes());
        hasher.update(b"(");
        let mut separate = false;
        for item in self.fields() {
            if separate {
                hasher.update(b",");
            }
            separate = true;
            hasher.update(item.value.type_name().as_bytes());
            hasher.update(b" ");
            hasher.update(item.name.as_bytes());
        }
        assert!(separate, "a user action always carries hyperliquidChain");
        hasher.update(b")");
        hasher.finalize().into()
    }

    /// Signs this action.
    pub fn sign<A: Signer>(&self, signer: &A) -> Signature {
        signer.sign_digest(&self.digest())
    }
}

impl Walk for UserAction<'_> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        let count = self.fields().filter(|item| !item.omitted_from_body()).count();
        assert!(count > 0);
        assert!(count <= USER_FIELDS_MAX);
        sink.map(count as u32 + 2);
        entry_str(sink, "type", self.action_type);
        entry_str(sink, "signatureChainId", USER_CHAIN_ID_HEX);
        for item in self.fields() {
            if !item.omitted_from_body() {
                sink.key(item.name);
                item.value.walk(sink);
            }
        }
    }
}

/// The `multiSig` action's payload, the part each signer's signature covers.
#[derive(Clone, Copy, Debug)]
pub struct MultiSigPayload<'a, W: Walk> {
    pub multi_sig_user: Address,
    pub outer_signer: Address,
    pub action: &'a W,
}

/// The `multiSig` action, without its `type` tag when hashed: the reference hashes it stripped.
#[derive(Clone, Copy, Debug)]
pub struct MultiSigAction<'a, W: Walk> {
    pub payload: MultiSigPayload<'a, W>,
    pub signatures: &'a [Signature],
    /// Whether to emit the tag. The hash omits it and the body includes it.
    pub tagged: bool,
}

/// The most signers a multi-sig user can have, per the docs.
pub const MULTI_SIG_SIGNERS_MAX: usize = 10;

impl<W: Walk> Walk for MultiSigAction<'_, W> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        assert_ne!(self.signatures, []);
        assert!(self.signatures.len() <= MULTI_SIG_SIGNERS_MAX);
        sink.map(if self.tagged { 4 } else { 3 });
        if self.tagged {
            entry_str(sink, "type", "multiSig");
        }
        entry_str(sink, "signatureChainId", USER_CHAIN_ID_HEX);
        sink.key("signatures");
        sink.array(self.signatures.len() as u32);
        for signature in self.signatures {
            walk_signature(signature, sink);
        }
        sink.key("payload");
        sink.map(3);
        entry_str(sink, "multiSigUser", self.payload.multi_sig_user.to_hex().as_str());
        entry_str(sink, "outerSigner", self.payload.outer_signer.to_hex().as_str());
        sink.key("action");
        self.payload.action.walk(sink);
    }
}

/// What each inner signer of an L1 multi-sig signs: the tuple `[user, outer, action]`.
#[derive(Clone, Copy, Debug)]
pub struct MultiSigL1Tuple<'a, W: Walk> {
    pub multi_sig_user: Address,
    pub outer_signer: Address,
    pub action: &'a W,
}

impl<W: Walk> Walk for MultiSigL1Tuple<'_, W> {
    fn walk<S: Sink>(&self, sink: &mut S) {
        sink.array(3);
        sink.str(self.multi_sig_user.to_hex().as_str());
        sink.str(self.outer_signer.to_hex().as_str());
        self.action.walk(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::JsonSink;

    fn json<W: Walk>(value: &W) -> std::string::String {
        let mut buffer = [0u8; 2048];
        let mut sink = JsonSink::new(&mut buffer);
        value.walk(&mut sink);
        let size = sink.finish().unwrap();
        std::string::String::from_utf8(buffer[..size].to_vec()).unwrap()
    }

    fn order() -> OrderWire {
        OrderWire {
            asset: Asset::perp(4),
            side: Side::Buy,
            px: Px::parse(b"1670.1").unwrap(),
            sz: Sz::parse(b"0.0147").unwrap(),
            reduce_only: false,
            kind: OrderKind::Limit(Tif::Ioc),
            cloid: None,
        }
    }

    #[test]
    fn order_body_matches_the_documented_shape() {
        let orders = [order()];
        let action = Action::Order { orders: &orders, grouping: Grouping::Na, builder: None };
        assert_eq!(
            json(&action),
            r#"{"type":"order","orders":[{"a":4,"b":true,"p":"1670.1","s":"0.0147","r":false,"t":{"limit":{"tif":"Ioc"}}}],"grouping":"na"}"#
        );
    }

    #[test]
    fn priority_grouping_and_builder_take_their_documented_forms() {
        let orders = [order()];
        let builder = Builder { address: Address([0xab; 20]), fee: 10 };
        let action = Action::Order {
            orders: &orders,
            grouping: Grouping::Priority(80_000),
            builder: Some(builder),
        };
        let text = json(&action);
        assert!(text.contains(r#""grouping":{"p":80000}"#));
        assert!(
            text.ends_with(
                r#""builder":{"b":"0xabababababababababababababababababababab","f":10}}"#
            )
        );
    }

    #[test]
    fn user_action_carries_its_chain_and_equal_nonce() {
        let action = UserAction::usd_send(
            Network::Testnet,
            Address([0x11; 20]),
            Sz::parse(b"1").unwrap(),
            1_687_816_341_423,
        );
        assert_eq!(action.nonce_ms(), 1_687_816_341_423);
        assert_eq!(
            json(&action),
            r#"{"type":"usdSend","signatureChainId":"0x66eee","hyperliquidChain":"Testnet","destination":"0x1111111111111111111111111111111111111111","amount":"1","time":1687816341423}"#
        );
    }

    #[test]
    fn the_same_action_on_two_networks_signs_differently() {
        let destination = Address([0x11; 20]);
        let amount = Sz::parse(b"1").unwrap();
        let main = UserAction::usd_send(Network::Mainnet, destination, amount, 1);
        let test = UserAction::usd_send(Network::Testnet, destination, amount, 1);
        assert_ne!(main.digest(), test.digest());
    }

    #[test]
    fn multi_sig_fields_go_right_after_the_chain() {
        let base = UserAction::c_deposit(Network::Mainnet, 5, 9);
        let enriched = base.for_multi_sig(Address([1; 20]), Address([2; 20]));
        let names: std::vec::Vec<&str> = enriched.fields().map(|item| item.name).collect();
        assert_eq!(
            names,
            ["hyperliquidChain", "payloadMultiSigUser", "outerSigner", "wei", "nonce"]
        );
        assert_ne!(base.digest(), enriched.digest());
    }
}
