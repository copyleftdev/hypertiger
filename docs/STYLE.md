# Style

hypertiger follows [TigerStyle](TIGER_STYLE.upstream.md), TigerBeetle's own document, fetched
2026-11-05. Its priorities are safety, then performance, then developer experience. This page records
how each rule maps onto Rust, how each is enforced, and where this project falls short of it.

Heavy steps use 4 cores at most (`xtask`'s `JOBS`) and mutation sweeps use 2
(`.cargo/bounded-mutants.env`). Run one heavy job at a time: this machine has reset under more.

A rule nothing enforces is a wish, so every rule below names what enforces it. `cargo run -p xtask --
gate` runs all of them.

| TigerStyle | Here | Enforced by |
|------------|------|-------------|
| No allocation after startup | The kernel is `#![no_std]` with no `alloc` crate. It cannot reach a heap | **The compiler**, on `x86_64-unknown-none`, with no `alloc` or `std` feature anywhere in its dependency graph |
| Assertions stay on | `assert!`, never `debug_assert!`. Release keeps `overflow-checks` | `Cargo.toml` profile |
| Two assertions per function | See [the gap below](#where-this-falls-short) | `xtask style`, by module |
| A limit on everything | Every size, count and capacity is a named constant whose comment says why | Compile-time assertions tie them together |
| No recursion | The JSON reader uses a fixed stack, so `[[[[...` is an error | `xtask style` finds call cycles. A lint, not a proof: see below |
| 70 lines per function | | Clippy `too_many_lines` |
| 100 columns | | `rustfmt` and `xtask style` |
| Strictest warnings | Clippy pedantic, warnings are errors | Gate |
| Explicitly sized integers | `u32` and `u64`; `usize` only at slice boundaries | Compile-time assertion that `usize` is 64 bits |
| No floats (added here) | A price never passes through one | `clippy.toml` `disallowed-types`, `float_arithmetic = "deny"` and `xtask style` |
| Handle every error | Operating errors are enums. Programmer errors are assertions | `#[must_use]`, `unused_must_use = "deny"` |
| Options at the call site | `EngineConfig` has no `Default`. Each value that changes what happens to money is written where the engine is built | No `Default` impl exists |
| Don't act on external events | `on_*` records. Work leaves through `poll_transmit`, on a tick | Tests in `tests/engine.rs` |
| Tools in the project's language | The gate, style checks and canaries are Rust | `xtask/` |
| Deterministic simulation | Same seed, same run, down to a trace digest | `--determinism` in the gate |
| Zero dependencies | See below | `xtask` allow-list |

## What an assertion means here

- **Never assert on bytes from the network.** A malformed reply is an operating error, and it is
  returned or treated as "unknown". Assertions are for states the code could not reach without a bug.
  This is the rule most easily broken without noticing. An independent review found five ways the wire
  reached an assertion or an overflow, and one was an ordinary mainnet order: an assertion about a
  value that passes through a parser is still an assertion about the network.
- **Not being able to read something is never the same as there being nothing there.** A read that
  cannot be fully understood is refused and asked again. Recovery does not finish on one.
- **An assertion must be able to fail on a real bug.** Restating the line above does not count, and
  Clippy rejects constant ones.
- **Pair them.** `status_due` keeps an ambiguous order from being queried early, and `status_replied`
  asserts that it was not. A signature is recovered before it leaves, and the model exchange recovers
  it again from the body it received.
- **Negative space.** Status words nobody has read stay live. Replies that cannot be lined up with their
  batch decide nothing. A frame with reserved bits set ends the connection.

## Dependencies

TigerBeetle allows none. Rust's standard library has no secp256k1, and hand-rolling a signature scheme
would trade supply-chain risk for a far more likely catastrophe. So:

- **Kernel:** `k256` and `sha3`, both RustCrypto. Cryptography only, and never hand-rolled. SHA-1 and
  base64 are written out in `ws.rs` because the WebSocket handshake needs them and they carry no
  security there: TLS does.
- **Edge:** `rustls` and `webpki-roots`. There is no TLS without them.
- **No async runtime.** `Session::step` does bounded work and returns, on the caller's tick.
- **The tree is not zero.** `k256` brings 25 crates. That is the price of not writing elliptic curves,
  and each of them is `no_std` with no allocator.

## Rust specifics

- **`unsafe` is forbidden** across the workspace.
- **Large structs move once.** TigerStyle builds them in place through an out pointer, which in Rust
  takes `unsafe`. `Engine::new` passes its arrays through the stack once at startup, so call it on a
  thread with a normal stack, and box the result.
- **Names.** Units and qualifiers go last: `latency_ms_max` becomes `order_ttl_ms`, `size_lots_max`,
  `weight_per_minute`. Related names share a length where they can: `source` and `target`, not `src`
  and `dest`.
- **Canaries over coverage.** Each canary plants one bug in a copy of the tree, and the named detector
  must fail. A canary whose target text has moved fails too, so a refactor cannot silently retire one.
- **The campaign checks itself.** It fails if any rare branch was never entered: a run that never landed
  an order past its timeout says nothing about that path.

## Where this falls short

**Two assertions per function is met by the stateful core, not by the whole kernel.**

| Scope | Density | Held to |
|-------|---------|---------|
| Core: `engine`, `limits`, `market`, `book`, `nonce` | 2.0 | 2.0, enforced |
| Whole kernel | about 0.89 | A floor of 0.80, which only moves up |

The modules below the line are `feed`, which reads untrusted bytes and so has nothing it may assert on;
`action`, `info` and `sink`, walks of a fixed shape; and `decimal`, `json`, `http`, `ws`, `signing` and
`types`, codecs and small value types. Padding them to a number would be exactly the filler the rule
exists to prevent. They are held by what can see them better:

- `action` and `signing`: 178 vectors from the official SDK, and the live exchange recovering the signer.
- `decimal`: exhaustive bands, 4,000 values against Python's `Decimal`.
- `json`, `http`, `ws`: every strict prefix refused, the RFC's own vectors, and refusal tables.
- `feed`: traffic captured from mainnet.

That is a judgment call and a departure from TigerStyle as written. It is recorded here so that it can
be argued with.

**Recursion checking is a lint.** It resolves `self.f(`, `Type::f(` and bare `f(`, and it cannot follow a
call through a generic receiver. `action.walk(sink)` is exactly that case. It is bounded separately:
`Sink` panics past `NESTING_MAX`.

**The edge is not held to the kernel's rules.** `hypertiger-net` allocates once per connection, and
`rustls` allocates per record. The zero-allocation guarantee stops at that crate's boundary.
