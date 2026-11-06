#!/usr/bin/env python3
"""Turns one traced simulator run into the data behind the landing page's film plate.

Nothing is invented here: every field below is read from the trace of a real seed, so the plate
replays that run and anyone can rebuild it.

Usage:
  cargo run --release -p hypertiger-vopr -- --trace 23 \
    | python3 tools/plate.py 23 > site/plate.json
"""
import json
import re
import sys

ONE = 100_000_000  # The trace prints sizes at 1e8.
LINE = re.compile(r"^\s*(\d+)\.(\d\d) (\w+) ?(.*)$")
ORDER = re.compile(
    r'\{"a":(\d+),"b":(true|false),"p":"[^"]*","s":"([^"]*)","r":(true|false),'
    r'"t":\{[^}]*\{?[^}]*\}?\},"c":"(0x[0-9a-f]+)"\}'
)
FINISH = re.compile(r"cloid: Some\((0x[0-9a-f]+)\), sz_filled: Sz\((\d+)\), finish: (\w+)")
AMBIGUOUS = re.compile(r"Ambiguous \{ cloid: (0x[0-9a-f]+)")
RESTING = re.compile(r"Resting \{ cloid: (0x[0-9a-f]+)")
FILLED = re.compile(r"Filled \{ cloid: Some\((0x[0-9a-f]+)\)")
EXPIRES = re.compile(r'"expiresAfter":(\d+)')
EPOCH_MS = 1_700_000_000_000


def read_coverage(text):
    return {name: int(value) for name, value in re.findall(r"^\s+(\w+): (\d+),$", text, re.M)}


def parse(text):
    orders = {}
    bounds = {"BTC": [], "ETH": []}
    for raw in text.splitlines():
        match = LINE.match(raw)
        if not match:
            continue
        now = int(match[1]) * 1000 + int(match[2]) * 10
        kind, rest = match[3], match[4]
        if kind == "send" and '"type":"order"' in rest:
            fate = rest.split(" ", 2)[1]
            expires = EXPIRES.search(rest)
            for found in ORDER.finditer(rest):
                orders.setdefault(found[5], {}).update(
                    sent=now,
                    fate=fate,
                    market=int(found[1]),
                    buy=found[2] == "true",
                    sz=float(found[3]),
                    reduce=found[4] == "true",
                    deadline=int(expires[1]) - EPOCH_MS if expires else None,
                )
        elif kind == "bound":
            coin, long_true, short_true, long_bound, short_bound = rest.split()
            bounds[coin].append(
                [now, *(round(int(v) / ONE, 3) for v in (long_true, short_true, long_bound, short_bound))]
            )
        elif kind == "event":
            note_event(orders, now, rest)
    return orders, bounds


def note_event(orders, now, rest):
    for pattern, field in ((AMBIGUOUS, "doubt"), (RESTING, "rest"), (FILLED, "fill")):
        found = pattern.search(rest)
        if found:
            orders.setdefault(found[1], {}).setdefault(field, now)
    found = FINISH.search(rest)
    if found:
        order = orders.setdefault(found[1], {})
        order.setdefault("end", now)
        order.setdefault("finish", found[3])
        order.setdefault("filled", int(found[2]) / ONE)


def resolved_at(order):
    """When the doubt ended: the first thing the engine learned for certain."""
    candidates = [order.get(key) for key in ("rest", "fill", "end")]
    later = [time for time in candidates if time is not None and time >= order["doubt"]]
    return min(later) if later else None


def row(order):
    doubt = order.get("doubt")
    return {
        "t": order["sent"],
        "b": int(order["buy"]),
        "m": order["market"],
        "s": order["sz"],
        "r": int(order["reduce"]),
        "f": order.get("fate", "Normal"),
        "d": doubt,
        "x": order.get("deadline"),
        "v": resolved_at(order) if doubt is not None else None,
        "e": order.get("end"),
        "o": order.get("finish"),
        "z": order.get("filled"),
    }


def main():
    seed = int(sys.argv[1])
    text = sys.stdin.read()
    orders, bounds = parse(text)
    rows = sorted((row(o) for o in orders.values() if "sent" in o), key=lambda r: r["t"])
    json.dump(
        {
            "seed": seed,
            "failed": re.search(r"^None\nCoverage", text, re.M) is None,
            "coverage": read_coverage(text),
            "settings": {"timeout_ms": 1500, "ttl_ms": 2000, "margin_ms": 1000, "limit_sz": 5},
            "orders": rows,
            "bounds": bounds,
        },
        sys.stdout,
        separators=(",", ":"),
    )


if __name__ == "__main__":
    main()
