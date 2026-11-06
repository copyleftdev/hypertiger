# Design

What this kernel guarantees, what those guarantees rest on, and what has not been verified.

## The problem

Every Hyperliquid client has the same hole in it. A request times out, and it has one of three
fates: it never arrived, it landed and the reply was lost, or it is still travelling and will land
later. Retrying can double an order. Assuming failure understates risk. The SDKs hand that choice to
the caller, and the caller usually gets it wrong at the worst moment, because timeouts cluster when
the market is moving.

## The idea

**An order carries `expiresAfter`, so the third fate has a deadline.** After the deadline, the outcome
can be settled for certain by asking about the client's own order id. Until then the engine does not
guess:

- An ambiguous order counts against the risk limit, in full.
- It is never resent.
- Past `expiresAfter + settle_margin_ms`, `orderStatus` answers it: found means it landed, and
  `unknownOid` proves it never did.

A timeout is no longer a judgment call. It is a state with a known exit.

## Guarantees

Each of these is checked by the simulator's oracle at every step, against the model exchange's truth.

| # | Guarantee | Checked by |
|---|-----------|------------|
| G1 | **Bound.** Once recovery finishes, `long_bound` and `short_bound` hold across every world the network could have left behind. Before that, `exposure` answers `None`, since the exchange may hold anything | `check_bounds` |
| G2 | **Limit.** The account cannot pass its configured limit, whichever way open questions resolve | `check_limits` |
| G3 | **At most once.** The engine does not send a client id twice, within one process and within `CLOIDS_SEEN_MAX` finished orders. **Across restarts this rests on the caller** choosing ids that do not repeat | model, on landing |
| G4 | **Proof.** `NeverLanded` is never reported for an order that landed, or that lands afterward | model and `note_finish` |
| G5 | **Nonces.** Strictly increasing within a process, whatever the clock does. A restart with an earlier clock could repeat one, which the exchange refuses | model, 100-highest rule |
| G6 | **Signature.** The body sent and the hash signed cannot disagree | model re-encodes the JSON it received |
| G7 | **Budget.** No sliding minute passes the exchange's 1,200 weight, across crashes, counting the surcharge on fill lists | `charge_weight` |
| G8 | **Convergence.** Once faults stop, every order resolves and positions match exactly | `check_converged` |
| G9 | **Replay.** A seed reproduces a run exactly | trace digest, compared across two runs |

## How it is built

```
  strategy ──► Engine ──► poll_transmit ──► driver ──► the exchange
                 ▲                            │
                 └── on_reply / on_message ◄──┘
```

The engine is **sans-IO**. It reads no clock, opens no socket and starts no thread; a driver passes in
time and bytes. That is what lets the simulator run the same code the live path runs, compressing an
hour of failures into a tenth of a second.

Following TigerStyle's rule against acting directly on external events, `on_*` only records what
happened. Work leaves at the engine's pace, in batches, once per tick: cancels first, then status
questions, then reads, then ALO orders, then IOC and GTC. Cancels may spend a reserve of rate limit
that nothing else may touch, so a client out of budget can still get out of the market.

### Crash-only recovery

There is no durable state, because **the exchange is the log**. Every start is a recovery:

1. **Quarantine** for `order_ttl_ms + settle_margin_ms`. An order signed by a previous life could land
   until then, so nothing is accepted.
2. **Read open orders**, and adopt them.
3. **Wait a margin, then read positions.** Two reads see two moments. An order finishing between them
   would be in neither, unless the second read is certain to be the later one.

There is no separate clean start to get wrong.

### Numbers without floats

The docs list trailing zeros and rounding among the ways signatures go wrong, and the official SDKs
pass prices through `f64` and guard the result with an epsilon. Here the wire string parses straight
to an integer, and a value that cannot be held exactly is an error. The ranges were measured on
mainnet rather than assumed: `totalSupply` reached 1.8e14, past what `u64` holds at 8 decimals, which
is why `Sz` is `u128`.

### One walk, two encodings

The exchange recovers the signer from a hash of the action's msgpack, and it reads the action from
JSON. A client that builds those separately can sign one thing and send another, and all it gets back
is "user does not exist". Here an action is one `walk` over a `Sink`. The hash and the body are the
same traversal, so they cannot disagree. Both sinks also check that a walk delivers what it declared,
because a miscounted map in msgpack is a valid-looking wrong hash.

### Self-correcting positions

Every fill carries `startPosition`, the exchange's own statement of the position just before it. A
fill from a newer block therefore restates the position outright, so a missed fill or a stale read
heals on the next one. Within one block fills add up, and an older fill is already inside the position
a newer one restated.

## What the simulator found

These were written as "correct" and failed under the VOPR or its canaries. They are recorded because
they say what kind of mistake this design invites.

| Found by | Defect |
|----------|--------|
| VOPR, first run | A late reply reached a different order that had since moved into the same slot. Requests now name a slot and its generation |
| VOPR, first run | The fill clock was global, so an ETH fill could hide an older BTC fill. It is per market |
| VOPR | A stale read overwrote a good position. `clearinghouseState`'s `time` says when it was answered, not how old its state is |
| VOPR | **A reduce-only order can remove the hedge a working order relied on.** Long 1.5 with 5 of sells looks like -3.5, but a reduce-only sell closing the long leaves all 5 naked. Both are gated now |
| VOPR | A finished order's unseen fills were freed on a timer, understating the bound until the next check. Only a read provably new enough ends that wait now |
| VOPR | Recovery could replace a position a newer fill had already corrected, and could count fills twice against an adopted order's remainder |
| VOPR | Open orders and positions read together could each miss an order that finished between them |
| Canary | Fill-list order was guessed from timestamps, which fails on a list from one block. The caller knows the source, so now it says |
| Canary | A regression test for the slot bug passed with the bug present, because a stale *cancel* reply happens to be harmless. The dangerous case is a stale *rejection* freeing a live order's exposure |

An independent adversarial review then found what the simulator could not, because the simulator shared
the assumption each one broke. These are the more instructive list.

| Defect | Why the simulator missed it |
|--------|-----------------------------|
| **A TP/SL set on a whole position crashed the engine at every start.** Mainnet lists them with `origSz` of 0.0, and adopting one hit an assertion. Resting orders would have stayed open with nothing able to cancel them | The model only held the engine's own orders, always sized |
| An error where a list belonged read as an empty list, and an unparseable position as flat. Recovery could finish believing the account held nothing | Its only malformed replies were cut in half, so they never parsed |
| `time: 0`, a fill time near `u64::MAX`, `oid: 0` and `statuses` as an object each reached an assertion or an overflow | The same: no well-formed strangeness |
| A half-open socket went unnoticed for about 15 minutes, during which cancels went nowhere | The simulator stops at the engine's API, and nothing simulates the socket |
| Any post error containing the digits `429` was read as a definite refusal, freeing exposure | Same |
| A `filled` status can carry a remainder: 45 cases in mainnet history. It was reported as fully filled | The model always called a remainder `canceled` |
| A backward clock step stopped cancels, and a forward one produced false proofs | Skew in the model was a constant |
| The budget ignored the per-item surcharge on `userFills`, and every restart got a full bucket, so a crash loop could pass the limit | The model charged a flat weight and reset it on crash |
| `user` set to a master account while trading a sub-account would answer `unknownOid` for real orders: a false proof | The model has one account |

The simulator now constructs the first group and the `filled` and budget cases. **The socket and clock
fixes remain outside it**, and they rest on review and reasoning.

## The assumptions

The guarantees are true of the model. They are true of Hyperliquid to the extent that these are.
**A failed assumption here means real money, so read this list before trusting the bound.**

| # | Assumption | Basis | If it is false |
|---|------------|-------|----------------|
| A1 | The exchange refuses an action past `expiresAfter`, measured against block time | Docs | **G1 and G4 break.** An order declared never-landed could land |
| A2 | `settle_margin_ms` exceeds clock skew to block time plus how far any API server's reads can lag | **Not measured.** Picked per deployment. The engine stops new orders if the exchange's clock disagrees by more than the margin, which covers skew and not read lag | **G4 breaks.** Stale `unknownOid` for an order that did land. Canary `no_margin` shows this is caught when the world violates it |
| A3 | `unknownOid` means "never seen", not "pruned" or "not on this server" | Observed on a fresh id; **pruning untested** | Old orders only. Ambiguities settle within seconds, so the exposure is narrow |
| A4 | `{"status":"err"}` and a per-order `error` mean nothing landed | Docs' description of pre-validation. **The model assumes the same**, so the oracle cannot test this | An order counted as dead may be live until reconcile adopts it |
| A5 | HTTP 429, and a post error whose text begins with `429`, mean not processed | Conventional, and the docs say post errors mirror the HTTP status; **no real one has been seen** | As A4 |
| A6 | `startPosition` is the position just before the fill, for perps and spot | One shape captured from a live perp fill; **semantics inferred, spot untested** | The position is wrong until a read corrects it |
| A7 | `/info` fill lists are newest first and feed snapshots oldest first, including within a block | Review measured 2,040 of 2,040 same-block groups across 6 accounts for `/info`, and 5 groups on 1 account for the feed | Same-block fills could be miscounted until a newer block restates |
| A8 | Fills carry `cloid` for orders placed with one | Captured traffic, 871 of 871 fills on one account; the docs omit the field | Matching falls back to `oid`, which is unknown until the reply |
| A9 | The caller's client ids do not repeat across restarts | The engine's memory of them is bounded and dies with the process. Exchange behavior on a repeat is **unknown** | A second order could land |
| A10 | The stale-read and skew bounds the simulator uses (500 ms and 300 ms against a 1,000 ms margin) | **Encoded by the harness, so the oracle cannot test them.** Only canary `no_margin` shows what happens beyond them | As A2 |

### Measuring A2 before going live

The margin is the most consequential number here, and nothing in this repository can tell you what it
should be for your deployment. Before trusting it, submit an order and poll `orderStatus` from a second
connection to see how long `unknownOid` persists after it rests. Do that repeatedly, under load, against
the server you will use.

## Not verified

Stated plainly, because a green gate reads as more than it is.

- **No order has been placed on a funded account.** The live checks use a throwaway key with no funds.
  Signatures, request shapes and the refusal path are proven against the real exchange. **Real fills,
  real `orderUpdates` and a real ambiguity resolving have never been seen by this code**, only their
  captured shapes and a model of their behavior.
- **The model is written from the docs and from observation.** Where it and Hyperliquid differ, the
  simulator is confidently testing the wrong thing. The simulator proves the engine correct against the
  model, not against the exchange.
- **`Session` and `conn` are the least-tested code here.** They have run against the real server in the
  normal case only. Reconnects, partial writes, fragmentation, the silence detector, the clock-step halt
  and the full-buffer check are reasoned about and not exercised, because the simulator stops at the
  engine's API and does not see `hypertiger-net`.
- **Lowering `order_ttl_ms` across a restart** ends quarantine before the previous life's longer expiries
  have passed. Quarantine trusts the current config.
- **Spot positions** are not loaded at recovery, because `clearinghouseState` covers only the default
  dex's perps. After a restart a spot market's position is wrong until its next fill restates it, and a
  limit on one is only as good as that. **Do not rely on the limit for spot or builder dex markets
  across a restart.**
- **Trigger orders, TP/SL and modifies** are typed and signed but not managed by the engine, which runs
  limit orders. Trigger orders found at recovery are adopted and counted against the bound.
- **The dead man's switch** is available as an action and is not armed by the engine.

## Performance

Measured service times, one thread, on a loaded machine (`crates/hypertiger-vopr/src/bin/bench.rs`).
These exclude queueing and the network.

| Operation | p50 | p99 |
|-----------|-----|-----|
| Decimal parse | 30 ns | 40 ns |
| Action hash, 1 order | 0.6 µs | 0.6 µs |
| Action hash, 39 orders | 16 µs | 23 µs |
| Request body, 39 orders | 11 µs | 19 µs |
| Parse and apply a 20-level book | 6.3 µs | 10 µs |
| Parse `allMids` (25 KB) | 40 µs | 49 µs |
| Sign, including the recovery check | 245 µs | 316 µs |

The docs put a cancel or ALO order at about 380 ms end to end, which is what makes the 177 µs recovery
check on every signature affordable: it adds 0.05% to the slowest path, and it turns a flipped bit into
a crash rather than a rejected order. A full `Engine<1024, 8>` is about 470 KB, inline and allocated once.
