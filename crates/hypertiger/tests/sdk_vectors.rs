//! Holds every action against the official Python SDK, the reference implementation.
//!
//! `tools/vectors.py` drives the SDK's real `Exchange` class and records what it would have sent.
//! This test rebuilds each action from typed values, independently, and requires the same action
//! hash, the same signature and, for L1 actions, the same request body byte for byte. A field out
//! of order, a number in the wrong form, or a byte wrong in the framing all fail here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hypertiger::action::{
    Action, AssetTransfer, Builder, CancelCloidWire, CancelWire, Envelope, Grouping, ModifyWire,
    MultiSigAction, MultiSigL1Tuple, MultiSigPayload, OrderKind, OrderRef, OrderWire, Tif, Tpsl,
    UserAction,
};
use hypertiger::decimal::{Px, Sz};
use hypertiger::market::Asset;
use hypertiger::signing::{Signature, Signer, Wallet, l1_action_hash, l1_digest};
use hypertiger::sink::{JsonSink, Walk};
use hypertiger::types::{Address, Cloid, Network, Side};

const KEY: [u8; 32] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01,
    0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23,
];
const NONCE: u64 = 1_700_000_000_123;
const ETH: Asset = Asset::from_raw(1);
const BTC: Asset = Asset::from_raw(0);
const TOKEN: &str = "PURR:0xc4bf3f870c0e9465323c0b6ed28096c2";

fn px(text: &str) -> Px {
    Px::parse(text.as_bytes()).unwrap()
}

fn sz(text: &str) -> Sz {
    Sz::parse(text.as_bytes()).unwrap()
}

fn destination() -> Address {
    Address::parse(b"0x5e9ee1089755c3435139848e47e6635505d5a13a").unwrap()
}

fn vault() -> Address {
    Address::parse(b"0x1719884eb866cb12b2287399b15f7db5e7d775ea").unwrap()
}

fn cloid() -> Cloid {
    Cloid::parse(b"0x00000000000000000000000000000001").unwrap()
}

fn agent() -> Address {
    Wallet::from_bytes(&[0x11; 32]).unwrap().address()
}

fn limit(asset: Asset, side: Side, sz_text: &str, px_text: &str, tif: Tif) -> OrderWire {
    OrderWire {
        asset,
        side,
        px: px(px_text),
        sz: sz(sz_text),
        reduce_only: false,
        kind: OrderKind::Limit(tif),
        cloid: None,
    }
}

/// Hands an L1 action to `check`. The slices live on this frame, which is why this is a callback
/// and not a return value.
fn with_l1_action(name: &str, check: &mut dyn FnMut(&Action<'_>)) -> bool {
    let gtc = limit(ETH, Side::Buy, "0.0147", "1670.1", Tif::Gtc);
    match name {
        "order_gtc" => order_action(&[gtc], None, check),
        "order_ioc_sell_reduce" => {
            let order =
                OrderWire { reduce_only: true, ..limit(BTC, Side::Sell, "0.5", "90000", Tif::Ioc) };
            order_action(&[order], None, check);
        }
        "order_alo_cloid" => {
            let order =
                OrderWire { cloid: Some(cloid()), ..limit(ETH, Side::Buy, "100", "100", Tif::Alo) };
            order_action(&[order], None, check);
        }
        "order_trigger" => {
            let order = OrderWire {
                reduce_only: true,
                kind: OrderKind::Trigger {
                    is_market: true,
                    trigger_px: px("3100.5"),
                    tpsl: Tpsl::Sl,
                },
                ..limit(ETH, Side::Sell, "1.25", "3100", Tif::Gtc)
            };
            order_action(&[order], None, check);
        }
        "order_builder" => {
            let builder = Builder { address: destination(), fee: 10 };
            order_action(&[limit(ETH, Side::Buy, "1", "2000", Tif::Gtc)], Some(builder), check);
        }
        "orders_two" => {
            let second = OrderWire {
                cloid: Some(cloid()),
                ..limit(BTC, Side::Sell, "0.1", "100000", Tif::Alo)
            };
            order_action(&[limit(ETH, Side::Buy, "1", "2000", Tif::Gtc), second], None, check);
        }
        "cancel" => {
            let cancels = [CancelWire { asset: ETH, oid: 77_738_308 }];
            check(&Action::Cancel { cancels: &cancels, fast: false });
        }
        "cancel_cloid" => {
            let cancels = [CancelCloidWire { asset: BTC, cloid: cloid() }];
            check(&Action::CancelByCloid { cancels: &cancels, fast: false });
        }
        "modify_oid" => {
            let modifies = [ModifyWire {
                target: OrderRef::Oid(77_738_308),
                order: limit(ETH, Side::Buy, "2", "2100", Tif::Gtc),
            }];
            check(&Action::BatchModify { modifies: &modifies });
        }
        "modify_cloid" => {
            let modifies = [ModifyWire {
                target: OrderRef::Cloid(cloid()),
                order: OrderWire {
                    reduce_only: true,
                    cloid: Some(cloid()),
                    ..limit(ETH, Side::Sell, "2", "2100", Tif::Ioc)
                },
            }];
            check(&Action::BatchModify { modifies: &modifies });
        }
        _ => return with_flat_action(name, check),
    }
    true
}

fn order_action(
    orders: &[OrderWire],
    builder: Option<Builder>,
    check: &mut dyn FnMut(&Action<'_>),
) {
    check(&Action::Order { orders, grouping: Grouping::Na, builder });
}

fn with_flat_action(name: &str, check: &mut dyn FnMut(&Action<'_>)) -> bool {
    let action = match name {
        "schedule_cancel_unset" => Action::ScheduleCancel { time_ms: None },
        "schedule_cancel_set" => Action::ScheduleCancel { time_ms: Some(1_700_000_100_000) },
        "update_leverage" => Action::UpdateLeverage { asset: ETH, is_cross: true, leverage: 21 },
        "update_leverage_isolated" => {
            Action::UpdateLeverage { asset: BTC, is_cross: false, leverage: 3 }
        }
        "update_margin_add" => Action::UpdateIsolatedMargin { asset: ETH, ntli: 100_000_000 },
        "update_margin_remove" => Action::UpdateIsolatedMargin { asset: ETH, ntli: -1_500_000 },
        "noop" => Action::Noop,
        "create_sub_account" => Action::CreateSubAccount { name: "example" },
        "set_referrer" => Action::SetReferrer { code: "CODE" },
        "sub_account_transfer" => {
            Action::SubAccountTransfer { sub_account: destination(), is_deposit: true, usd: 10 }
        }
        "sub_account_spot_transfer" => Action::SubAccountSpotTransfer {
            sub_account: destination(),
            is_deposit: false,
            token: TOKEN,
            amount: sz("1.5"),
        },
        "vault_transfer" => {
            Action::VaultTransfer { vault: vault(), is_deposit: true, usd: 1_000_000 }
        }
        "evm_user_modify" => Action::EvmUserModify { using_big_blocks: true },
        "agent_enable_dex_abstraction" => Action::AgentEnableDexAbstraction,
        "agent_set_abstraction" => Action::AgentSetAbstraction { abstraction: "u" },
        "gossip_priority_bid" => {
            Action::GossipPriorityBid { slot_id: 1, ip: "1.2.3.4", max_gas: 100_000_000 }
        }
        _ => return false,
    };
    check(&action);
    true
}

fn with_user_action(name: &str, network: Network, check: &mut dyn FnMut(&UserAction<'_>)) {
    let action = match name {
        "usd_send" => UserAction::usd_send(network, destination(), sz("1"), NONCE),
        "spot_send" => UserAction::spot_send(network, destination(), TOKEN, sz("0.5"), NONCE),
        "withdraw" => UserAction::withdraw(network, destination(), sz("2.5"), NONCE),
        "usd_class_transfer" => UserAction::usd_class_transfer(network, "1.5", true, NONCE),
        "send_asset" => UserAction::send_asset(
            network,
            &AssetTransfer {
                destination: destination(),
                source_dex: "",
                destination_dex: "spot",
                token: "USDC",
                amount: sz("1.25"),
                from_sub_account: None,
            },
            NONCE,
        ),
        "token_delegate" => UserAction::token_delegate(network, destination(), 1000, false, NONCE),
        "approve_agent" => UserAction::approve_agent(network, agent(), "bot", NONCE),
        "approve_agent_unnamed" => UserAction::approve_agent(network, agent(), "", NONCE),
        "approve_builder_fee" => {
            UserAction::approve_builder_fee(network, "0.001%", destination(), NONCE)
        }
        "convert_to_multi_sig" => UserAction::convert_to_multi_sig(
            network,
            r#"{"authorizedUsers": ["0x1719884eb866cb12b2287399b15f7db5e7d775ea", "0x5e9ee1089755c3435139848e47e6635505d5a13a"], "threshold": 2}"#,
            NONCE,
        ),
        "user_dex_abstraction" => {
            UserAction::user_dex_abstraction(network, destination(), true, NONCE)
        }
        "user_set_abstraction" => {
            UserAction::user_set_abstraction(network, destination(), "unifiedAccount", NONCE)
        }
        _ => panic!("no user action named {name}: the vector file has a case this test lacks"),
    };
    check(&action);
}

fn hex32(text: &str) -> [u8; 32] {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    assert!(digits.len() <= 64, "{text}");
    let padded = format!("{digits:0>64}");
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&padded[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}

fn signature_from(r: &str, s: &str, v: &str) -> Signature {
    Signature { r: hex32(r), s: hex32(s), v: v.parse().unwrap() }
}

fn body_of<W: Walk>(value: &W) -> String {
    let mut buffer = vec![0u8; 8192];
    let mut sink = JsonSink::new(&mut buffer);
    value.walk(&mut sink);
    let size = sink.finish().unwrap();
    String::from_utf8(buffer[..size].to_vec()).unwrap()
}

struct Case<'a> {
    name: &'a str,
    network: Network,
    vault: Option<Address>,
    expires_after_ms: Option<u64>,
    nonce: u64,
    hash: &'a str,
    signature: Signature,
    body: &'a str,
}

fn parse_case(line: &str) -> (Case<'_>, &str) {
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(fields.len(), 11, "{line}");
    let case = Case {
        name: fields[0],
        network: if fields[2] == "mainnet" { Network::Mainnet } else { Network::Testnet },
        vault: Address::parse(fields[3].as_bytes()),
        expires_after_ms: fields[4].parse().ok(),
        nonce: fields[5].parse().unwrap(),
        hash: fields[6],
        signature: signature_from(fields[7], fields[8], fields[9]),
        body: fields[10],
    };
    (case, fields[1])
}

fn check_l1(case: &Case<'_>, wallet: &Wallet, action: &Action<'_>) {
    let hash = l1_action_hash(action, case.nonce, case.vault, case.expires_after_ms);
    assert_eq!(hash, hex32(case.hash), "{}: action hash", case.name);
    let signature = wallet.sign_digest(&l1_digest(&hash, case.network));
    assert_eq!(signature, case.signature, "{}: signature", case.name);
    let envelope = Envelope {
        action,
        nonce: case.nonce,
        signature,
        vault: case.vault,
        expires_after_ms: case.expires_after_ms,
    };
    assert_eq!(body_of(&envelope), case.body, "{}: body", case.name);
}

#[test]
fn every_sdk_action_matches() {
    let wallet = Wallet::from_bytes(&KEY).unwrap();
    let text = include_str!("vectors/actions.tsv");
    let mut checked_l1 = 0u32;
    let mut checked_user = 0u32;
    for line in text.lines() {
        let (case, scheme) = parse_case(line);
        if scheme == "l1" {
            let found = with_l1_action(case.name, &mut |action| {
                check_l1(&case, &wallet, action);
                checked_l1 += 1;
            });
            assert!(found, "no L1 action named {}", case.name);
        } else {
            with_user_action(case.name, case.network, &mut |action| {
                assert_eq!(action.sign(&wallet), case.signature, "{}: signature", case.name);
                assert_eq!(action.nonce_ms(), case.nonce);
                checked_user += 1;
            });
        }
    }
    // A vector file that silently shrank would pass the loop above.
    assert_eq!(checked_l1, 17 * 4 * 2 + 9 * 2);
    assert_eq!(checked_user, 12 * 2);
}

/// The vectors the SDK's own test suite pins, including the one marked as matching production.
#[test]
fn sdk_test_suite_vectors() {
    let order = limit(Asset::perp(4), Side::Buy, "0.0147", "1670.1", Tif::Ioc);
    let orders = [order];
    let action = Action::Order { orders: &orders, grouping: Grouping::Na, builder: None };
    let hash = l1_action_hash(&action, 1_677_777_606_040, None, None);
    assert_eq!(hash, hex32("0x0fcbeda5ae3c4950a548021552a4fea2226858c4453571bf3f24ba017eac2908"));

    let wallet = Wallet::from_bytes(&KEY).unwrap();
    let orders = [limit(Asset::perp(1), Side::Buy, "100", "100", Tif::Gtc)];
    let action = Action::Order { orders: &orders, grouping: Grouping::Na, builder: None };
    let hash = l1_action_hash(&action, 0, None, None);
    assert_eq!(
        wallet.sign_digest(&l1_digest(&hash, Network::Mainnet)),
        signature_from(
            "0xd65369825a9df5d80099e513cce430311d7d26ddf477f5b3a33d2806b100d78e",
            "0x2b54116ff64054968aa237c20ca9ff68000f977c93289157748a3162b6ea940e",
            "28",
        )
    );
    assert_eq!(
        wallet.sign_digest(&l1_digest(&hash, Network::Testnet)),
        signature_from(
            "0x82b2ba28e76b3d761093aaded1b1cdad4960b3af30212b343fb2e6cdfa4e3d54",
            "0x6b53878fc99d26047f4d7e8c90eb98955a109f44209163f52d8dc4278cbbd9f5",
            "27",
        )
    );
    let action = Action::CreateSubAccount { name: "example" };
    let hash = l1_action_hash(&action, 0, None, None);
    assert_eq!(
        wallet.sign_digest(&l1_digest(&hash, Network::Mainnet)),
        signature_from(
            "0x51096fe3239421d16b671e192f574ae24ae14329099b6db28e479b86cdd6caa7",
            "0xb71f7d293af92d3772572afb8b102d167a7cef7473388286bc01f52a5c5b423",
            "27",
        )
    );
}

#[test]
fn multi_sig_matches() {
    let wallet = Wallet::from_bytes(&KEY).unwrap();
    let line = include_str!("vectors/multisig.tsv").trim_end();
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(fields.len(), 9);

    let tuple = MultiSigL1Tuple {
        multi_sig_user: vault(),
        outer_signer: destination(),
        action: &Action::Noop,
    };
    let hash = l1_action_hash(&tuple, NONCE, None, None);
    let inner = wallet.sign_digest(&l1_digest(&hash, Network::Mainnet));
    assert_eq!(inner, signature_from(fields[1], fields[2], fields[3]), "inner");

    let signatures = [inner];
    let payload = MultiSigPayload {
        multi_sig_user: vault(),
        outer_signer: wallet.address(),
        action: &Action::Noop,
    };
    let stripped = MultiSigAction { payload, signatures: &signatures, tagged: false };
    let outer_hash = l1_action_hash(&stripped, NONCE, None, None);
    assert_eq!(outer_hash, hex32(fields[4]), "outer hash");

    let envelope = UserAction::multi_sig_envelope(Network::Mainnet, outer_hash, NONCE);
    let outer = envelope.sign(&wallet);
    assert_eq!(outer, signature_from(fields[5], fields[6], fields[7]), "outer signature");

    let tagged = MultiSigAction { payload, signatures: &signatures, tagged: true };
    let body = Envelope {
        action: &tagged,
        nonce: NONCE,
        signature: outer,
        vault: None,
        expires_after_ms: None,
    };
    assert_eq!(body_of(&body), fields[8], "body");
}

/// Number text agrees with Python's `Decimal.normalize()` across 4,000 generated values.
#[test]
fn wire_text_matches_python_decimal() {
    let mut checked = 0u32;
    for line in include_str!("vectors/wire.tsv").lines() {
        let (text, canonical) = line.split_once('\t').unwrap();
        assert_eq!(Sz::parse(text.as_bytes()).unwrap().to_wire().as_str(), canonical, "{text}");
        checked += 1;
    }
    assert_eq!(checked, 4008);
}

#[test]
fn a_zero_nonce_never_leaves_the_typed_path() {
    let result = std::panic::catch_unwind(|| {
        let wallet = Wallet::from_bytes(&KEY).unwrap();
        let options = hypertiger::action::L1Options {
            network: Network::Testnet,
            nonce: 0,
            vault: None,
            expires_after_ms: None,
        };
        hypertiger::action::sign_l1(&Action::Noop, &options, &wallet)
    });
    assert!(result.is_err());
}
