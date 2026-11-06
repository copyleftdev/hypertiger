# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Landing page: static files in `site/`, deployed to GitHub Pages through GitHub Actions, using Three.js or WebGL (the user's choice). The library itself is Rust.

## Users

Two audiences who share one problem. Rust engineers evaluating a Hyperliquid library, and market makers running bots on Hyperliquid. Both have seen, or fear, a request timing out with an order's fate unknown.

## Product Purpose

hypertiger is a Hyperliquid library for Rust in the style of TigerBeetle. Its order engine answers the question other clients leave to the caller: a request timed out, so what happened to the order? Success for now is engineers running the simulator and arguing with the design, because it is an early alpha.

## Positioning

An order sent with `expiresAfter` has a deadline by which it landed or never will. So a timed-out order counts in full against the risk limit, is never resent, and is settled for certain once the deadline and a margin have passed. The risk bound is checked by a deterministic simulator at every step.

## Operating Context

Evaluated by reading the repository, `docs/DESIGN.md`, and running `hypertiger-vopr` locally. Commands: `cargo run -p xtask -- gate`, `hypertiger-vopr --seeds N`, `hypertiger-vopr --trace SEED`, and the `ht` command line.

## Capabilities and Constraints

- Kernel: `no_std` with no `alloc`, no floats, sans-IO, depends only on `k256` and `sha3`.
- Every documented action is typed, except deployer and validator actions.
- Status: early alpha, tagged `v0.1.0-alpha.1`. Not ready for real money. No order has been placed on a funded account.
- The simulator proves the engine against a model of the exchange, not against Hyperliquid.
- `hypertiger-net`, the socket layer, has unit tests over a scripted stream only. TLS, the handshake and reconnect timing have no tests.
- No license has been chosen, so the page must not imply one.

## Brand Commitments

The name is hypertiger, lowercase. Public repository: https://github.com/copyleftdev/hypertiger. TigerStyle is TigerBeetle's document: the project follows it and is not affiliated with TigerBeetle or Hyperliquid.

## Evidence on Hand

Real and quotable, from this repository's own runs:
- 178 vectors from the official Python SDK match on hash and signature.
- The live testnet exchange recovered a throwaway signer on 52 action forms, with none wrong. Two more forms are refused for the official SDK too.
- 43 of 44 read requests and all 24 subscriptions accepted by the live exchange.
- 34 of 34 planted bugs caught, on the run before the socket refactor.
- Simulator, 6,000 seeds on an earlier build: 111,827 ambiguous orders, 98,077 proven never landed, 59,803 landing after their timeout, 0 failed. Later runs use harsher worlds.
- Service times on a loaded machine: sign 245 µs, parse and apply a book 6.3 µs.
- What the simulator found, and what a later independent review found that it could not: `docs/DESIGN.md`.

Absent, and not to be fabricated: users, testimonials, trading results, returns, funded-account evidence, benchmarks against other libraries.

## Product Principles

- Say what is not verified as plainly as what is.
- Show the mechanism working rather than claiming it.
- An unknown outcome is counted, never guessed.
- Every number on a public surface must trace to a run.
