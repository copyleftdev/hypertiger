# hypertiger

[![Tip my tokens](https://tokentip.to/badge/copyleftdev.svg?logo=1)](https://tokentip.to/@copyleftdev)

A Hyperliquid library for Rust in the style of [TigerBeetle](https://tigerbeetle.com): no allocation,
no floats, deterministic simulation, and assertions that stay on.

Its order engine answers the question every other client leaves to you: **a request timed out, so what
happened to the order?**

The simulator puts it through lost replies, crashes, stale reads and malformed answers, and checks its
bound against the model exchange's truth at every step. `hypertiger-vopr --seeds 6000` covers 200
simulated hours in about 90 seconds on 16 cores.

> **Status: early alpha, not ready for real money. No order has been placed on a funded account.** Signing and request
> shapes are proven against the live exchange, and the engine against a model of it. The socket layer is
> the least-tested code here. Read [what has not been verified](docs/DESIGN.md#not-verified) and
> [what it assumes](docs/DESIGN.md#the-assumptions).

## The idea

An order sent with `expiresAfter` has a deadline by which it either landed or never will. So a timeout
stops being a judgment call:

- The order keeps counting against your risk limit, in full.
- It is never resent.
- Past the deadline, asking about your own order id settles it for certain.

`long_bound` and `short_bound` hold across every world the network could have left behind. Every start
is a recovery: the engine waits out whatever a previous life could have left in flight, then adopts what
the exchange says is open. There is no local state to corrupt, because the exchange is the log.

## Crates

| Crate | What it is | Dependencies |
|-------|-----------|--------------|
| `hypertiger` | The kernel: decimals, signing, every action, JSON, book, engine, HTTP and WebSocket framing. `no_std`, no `alloc`, sans-IO | `k256`, `sha3` |
| `hypertiger-vopr` | The simulator, with a model exchange and an oracle | the kernel |
| `hypertiger-net` | The TLS edge, a `Session`, and the `ht` command | `rustls`, `webpki-roots` |
| `xtask` | The gate | none |

## Use

The kernel takes bytes and gives bytes, so it can sit under any transport.

```rust
use hypertiger::action::Tif;
use hypertiger::engine::{Engine, EngineConfig, Event, Intent, MarketConfig};
use hypertiger::market::{Asset, Market};
use hypertiger::types::{Cloid, Coin, Network, Side};

// No Default on purpose: each of these changes what can happen to money.
let config = EngineConfig {
    network: Network::Testnet,
    user: wallet.address(),
    vault: None,
    tick_ms: 100,
    order_ttl_ms: 5_000,
    settle_margin_ms: 2_000, // Measure this for your deployment: see docs/DESIGN.md.
    request_timeout_ms: 3_000,
    queue_ms_max: 2_000,
    status_poll_ms: 1_000,
    reconcile_ms: 30_000,
    fills_wait_ms: 5_000,
    weight_per_minute: 800,
    weight_burst: 300,
    weight_cancel_reserve: 50,
    fast_cancel: true,
};
let markets = [MarketConfig {
    coin: Coin::parse(b"BTC").unwrap(),
    market: Market::new(Asset::perp(0), 5).unwrap(),
    long_sz_max: Sz::parse(b"1").unwrap(),
    short_sz_max: Sz::parse(b"1").unwrap(),
}];
let engine: Box<Engine<1024, 8>> = Box::new(Engine::new(config, &markets, now_ms()));
let mut session = Session::new(SessionConfig { host: HOST_TESTNET, user }, engine, wallet);

loop {
    session.step(&mut |market_data| { /* books, trades, anything you subscribed to */ });

    while let Some(event) = session.poll_event() {
        match event {
            Event::Recovered => { /* orders are accepted from here */ }
            Event::Filled { sz, px, .. } => { /* new exposure, deduplicated */ }
            Event::Ambiguous { cloid } => { /* it counts; it will not be resent */ }
            Event::Finished { cloid, finish, .. } => { /* its slot is free */ }
            _ => {}
        }
    }

    if session.engine().is_running() {
        let intent = Intent {
            cloid: Cloid::from_parts(session_id, next_sequence()),
            market_index: 0,
            side: Side::Buy,
            tif: Tif::Alo,
            px,
            sz,
            reduce_only: false,
        };
        // Refusals cost nothing: no bytes have moved.
        session.engine_mut().submit(&intent, now_ms())?;
    }
}
```

For everything else there is a typed action, signed with one call.

```rust
let action = Action::UpdateLeverage { asset, is_cross: true, leverage: 5 };
let options = L1Options { network, nonce, vault: None, expires_after_ms: None };
let signature = sign_l1(&action, &options, &wallet);
let envelope = Envelope { action: &action, nonce, signature, vault: None, expires_after_ms: None };
```

## The `ht` command

```sh
ht book BTC                 # the live book
ht watch BTC --seconds 30   # stream it through the whole session stack
ht conformance              # have the testnet exchange recover the signer of every action
ht conformance-read         # send every read and subscription to the exchange
ht e2e                      # the full engine against testnet, with a throwaway key
```

None of these can move funds. `conformance` and `e2e` run only on testnet, with a key generated for that
run.

## The gate

```sh
cargo run -p xtask -- gate              # format, clippy, style, no_std, tests, VOPR, canaries
cargo run -p xtask -- canaries          # plant each bug, and require it to be caught
hypertiger-vopr --trace 1369            # replay a seed, printing every decisive event
```

The simulator is only worth what it can catch, so the gate plants bugs one at a time and fails if any
survives, and it fails a campaign that never entered a rare branch. It found a reply landing on the
wrong order, a reduce-only order silently removing the hedge a working order relied on, and several ways
recovery could understate risk. An independent review then found what it could not, such as an ordinary
position TP/SL crashing the engine at every start, and the simulator's worlds were widened to match.
See [what was found](docs/DESIGN.md#what-the-simulator-found).

## Documents

- [`docs/DESIGN.md`](docs/DESIGN.md): the guarantees, the assumptions, and what is not verified.
- [`docs/STYLE.md`](docs/STYLE.md): TigerStyle in Rust, what enforces each rule, and where this falls short.
- [`docs/COVERAGE.md`](docs/COVERAGE.md): every action and read, and what proves it.

## License

Not chosen yet, so for now no license is granted beyond reading the code here. One will be picked before
this leaves alpha.
