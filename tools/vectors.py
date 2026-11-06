#!/usr/bin/env python3
"""Generates golden vectors from the official Hyperliquid Python SDK.

The SDK is the reference implementation, so its output is the oracle. The real `Exchange` class is
driven with `post` stubbed out, which means the field orders below come from the SDK's own code and
not from a hand copy that could share this crate's mistakes.

Usage: python tools/vectors.py <path to hyperliquid-python-sdk> <output dir>
"""
import json
import random
import sys
from decimal import Decimal

sys.path.insert(0, sys.argv[1])

import eth_account  # noqa: E402
import hyperliquid.exchange as exchange_module  # noqa: E402
from hyperliquid.exchange import Exchange  # noqa: E402
from hyperliquid.utils import constants  # noqa: E402
from hyperliquid.utils.signing import action_hash, float_to_wire, sign_multi_sig_l1_action_payload  # noqa: E402
from hyperliquid.utils.types import Cloid  # noqa: E402

KEY = "0x0123456789012345678901234567890123456789012345678901234567890123"
NONCE = 1700000000123
VAULT = "0x1719884eb866cb12b2287399b15f7db5e7d775ea"
DESTINATION = "0x5e9ee1089755c3435139848e47e6635505d5a13a"
AGENT_KEY = "11" * 32
META = {"universe": [{"name": "BTC", "szDecimals": 5}, {"name": "ETH", "szDecimals": 4}]}
SPOT_META = {"universe": [], "tokens": []}
CLOID = Cloid.from_str("0x00000000000000000000000000000001")

exchange_module.get_timestamp_ms = lambda: NONCE
exchange_module.secrets.token_hex = lambda _size: AGENT_KEY


class Capture(Exchange):
    def post(self, _path, payload=None):
        self.captured = payload
        return {"status": "ok"}


def make(url, vault=None, expires=None):
    wallet = eth_account.Account.from_key(KEY)
    exchange = Capture(wallet, url, META, vault, None, SPOT_META)
    if expires is not None:
        exchange.set_expires_after(expires)
    return exchange


GTC = {"limit": {"tif": "Gtc"}}
IOC = {"limit": {"tif": "Ioc"}}
ALO = {"limit": {"tif": "Alo"}}
STOP = {"trigger": {"triggerPx": 3100.5, "isMarket": True, "tpsl": "sl"}}
BUILDER = {"b": DESTINATION, "f": 10}

L1 = {
    "order_gtc": lambda e: e.order("ETH", True, 0.0147, 1670.1, GTC),
    "order_ioc_sell_reduce": lambda e: e.order("BTC", False, 0.5, 90000.0, IOC, reduce_only=True),
    "order_alo_cloid": lambda e: e.order("ETH", True, 100.0, 100.0, ALO, cloid=CLOID),
    "order_trigger": lambda e: e.order("ETH", False, 1.25, 3100.0, STOP, reduce_only=True),
    "order_builder": lambda e: e.order("ETH", True, 1.0, 2000.0, GTC, builder=BUILDER),
    "orders_two": lambda e: e.bulk_orders([
        {"coin": "ETH", "is_buy": True, "sz": 1.0, "limit_px": 2000.0, "order_type": GTC, "reduce_only": False},
        {"coin": "BTC", "is_buy": False, "sz": 0.1, "limit_px": 100000.0, "order_type": ALO, "reduce_only": False, "cloid": CLOID},
    ]),
    "cancel": lambda e: e.cancel("ETH", 77738308),
    "cancel_cloid": lambda e: e.cancel_by_cloid("BTC", CLOID),
    "schedule_cancel_unset": lambda e: e.schedule_cancel(None),
    "schedule_cancel_set": lambda e: e.schedule_cancel(1700000100000),
    "modify_oid": lambda e: e.modify_order(77738308, "ETH", True, 2.0, 2100.0, GTC),
    "modify_cloid": lambda e: e.modify_order(CLOID, "ETH", False, 2.0, 2100.0, IOC, True, CLOID),
    "update_leverage": lambda e: e.update_leverage(21, "ETH", True),
    "update_leverage_isolated": lambda e: e.update_leverage(3, "BTC", False),
    "update_margin_add": lambda e: e.update_isolated_margin(100.0, "ETH"),
    "update_margin_remove": lambda e: e.update_isolated_margin(-1.5, "ETH"),
    "noop": lambda e: e.noop(NONCE),
}
L1_NO_VAULT = {
    "create_sub_account": lambda e: e.create_sub_account("example"),
    "set_referrer": lambda e: e.set_referrer("CODE"),
    "sub_account_transfer": lambda e: e.sub_account_transfer(DESTINATION, True, 10),
    "sub_account_spot_transfer": lambda e: e.sub_account_spot_transfer(DESTINATION, False, "PURR:0xc4bf3f870c0e9465323c0b6ed28096c2", "1.5"),
    "vault_transfer": lambda e: e.vault_usd_transfer(VAULT, True, 1000000),
    "evm_user_modify": lambda e: e.use_big_blocks(True),
    "agent_enable_dex_abstraction": lambda e: e.agent_enable_dex_abstraction(),
    "agent_set_abstraction": lambda e: e.agent_set_abstraction("u"),
    "gossip_priority_bid": lambda e: e.gossip_priority_bid(1, "1.2.3.4", 100000000),
}
# Amounts are strings: the SDK signs `str(amount)`, so a float would sign "1.0" where this crate's
# canonical form signs "1". The exchange accepts both, but they are different digests.
USER = {
    "usd_send": lambda e: e.usd_transfer("1", DESTINATION),
    "spot_send": lambda e: e.spot_transfer("0.5", DESTINATION, "PURR:0xc4bf3f870c0e9465323c0b6ed28096c2"),
    "withdraw": lambda e: e.withdraw_from_bridge("2.5", DESTINATION),
    "usd_class_transfer": lambda e: e.usd_class_transfer("1.5", True),
    "send_asset": lambda e: e.send_asset(DESTINATION, "", "spot", "USDC", "1.25"),
    "token_delegate": lambda e: e.token_delegate(DESTINATION, 1000, False),
    "approve_agent": lambda e: e.approve_agent("bot"),
    "approve_agent_unnamed": lambda e: e.approve_agent(None),
    "approve_builder_fee": lambda e: e.approve_builder_fee(DESTINATION, "0.001%"),
    "convert_to_multi_sig": lambda e: e.convert_to_multi_sig_user([VAULT, DESTINATION], 2),
    "user_dex_abstraction": lambda e: e.user_dex_abstraction(DESTINATION, True),
    "user_set_abstraction": lambda e: e.user_set_abstraction(DESTINATION, "unifiedAccount"),
}


def emit(out, name, network, vault, expires, exchange):
    payload = exchange.captured
    action = payload["action"]
    signature = payload["signature"]
    nonce = payload["nonce"]
    body = json.dumps(payload, separators=(",", ":"))
    scheme = "user" if "signatureChainId" in action else "l1"
    hashed = action_hash(action, payload["vaultAddress"], nonce, payload["expiresAfter"]).hex() if scheme == "l1" else "-"
    out.write("\t".join([
        name, scheme, network, vault or "-", str(expires or "-"), str(nonce), hashed,
        signature["r"], signature["s"], str(signature["v"]), body,
    ]) + "\n")


def actions(out):
    urls = [("mainnet", constants.MAINNET_API_URL), ("testnet", constants.TESTNET_API_URL)]
    for network, url in urls:
        for name, call in L1.items():
            for vault, expires in [(None, None), (VAULT, None), (None, 1700000060000), (VAULT, 1700000060000)]:
                exchange = make(url, vault, expires)
                call(exchange)
                emit(out, name, network, vault, expires, exchange)
        for name, call in {**L1_NO_VAULT, **USER}.items():
            exchange = make(url)
            call(exchange)
            emit(out, name, network, None, None, exchange)


def multi_sig(out):
    """The inner signature of an L1 multi-sig, then the outer action hashed and signed."""
    wallet = eth_account.Account.from_key(KEY)
    action = {"type": "noop"}
    inner = sign_multi_sig_l1_action_payload(wallet, action, True, None, NONCE, None, VAULT, DESTINATION)
    exchange = make(constants.MAINNET_API_URL)
    exchange.multi_sig(VAULT, action, [inner], NONCE)
    payload = exchange.captured
    outer = payload["action"]
    stripped = {key: value for key, value in outer.items() if key != "type"}
    out.write("\t".join([
        "multi_sig_noop", inner["r"], inner["s"], str(inner["v"]),
        action_hash(stripped, None, NONCE, None).hex(),
        payload["signature"]["r"], payload["signature"]["s"], str(payload["signature"]["v"]),
        json.dumps(payload, separators=(",", ":")),
    ]) + "\n")


def wires(out):
    """Canonical number text, by the same normalization `float_to_wire` applies after rounding."""
    rng = random.Random(1337)
    for _ in range(4000):
        whole = rng.choice([0, 0, 1, 9, 10, 100, rng.randrange(10**6), rng.randrange(10**12)])
        scale = rng.randrange(0, 9)
        fraction = rng.randrange(10**scale) if scale else 0
        text = f"{whole}.{fraction:0{scale}d}" if scale else f"{whole}"
        canonical = format(Decimal(text).normalize(), "f")
        out.write(f"{text}\t{canonical}\n")
    for value in [1670.1, 0.0147, 100.0, 1000.0, 3100.5, 0.00000001, 123456.0, 1.0e-7]:
        out.write(f"{format(Decimal(repr(value)), 'f')}\t{float_to_wire(value)}\n")


def main():
    target = sys.argv[2]
    with open(f"{target}/actions.tsv", "w") as out:
        actions(out)
    with open(f"{target}/multisig.tsv", "w") as out:
        multi_sig(out)
    with open(f"{target}/wire.tsv", "w") as out:
        wires(out)


main()
