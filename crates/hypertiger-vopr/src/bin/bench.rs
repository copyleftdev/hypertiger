//! Service times of the kernel's hot paths, on this machine, in this build.
//!
//! These are per-operation times on one idle thread with `Instant` on both sides of each call. They
//! say what the CPU costs and nothing about queueing or the network, which dominate an order's
//! end-to-end time by four orders of magnitude. The docs quote about 380 ms for a cancel or ALO
//! order to commit. That is why the design spends CPU on assertions.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::hint::black_box;
use std::time::Instant;

use hypertiger::action::{Action, Grouping, L1Options, OrderKind, OrderWire, Tif, sign_l1};
use hypertiger::book::Book;
use hypertiger::decimal::{Px, Sz};
use hypertiger::json::{Token, parse};
use hypertiger::market::Asset;
use hypertiger::signing::{Signer, Wallet, keccak, l1_action_hash, l1_digest, recover};
use hypertiger::sink::{JsonSink, Walk};
use hypertiger::types::{Cloid, Network, Side};

const ORDER_BOOK: &str = include_str!("../../../hypertiger/tests/fixtures/l2Book.json");
const ALL_MIDS: &str = include_str!("../../../hypertiger/tests/fixtures/allMids.json");

fn measure(name: &str, samples: u32, mut operation: impl FnMut()) {
    let mut times = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        let start = Instant::now();
        operation();
        times.push(start.elapsed().as_nanos());
    }
    times.sort_unstable();
    let at = |per_thousand: usize| times[(times.len() - 1) * per_thousand / 1000];
    println!(
        "{name:<34} p50 {:>9} ns   p99 {:>9} ns   p99.9 {:>9} ns   max {:>10} ns   n={samples}",
        at(500),
        at(990),
        at(999),
        times[times.len() - 1]
    );
}

fn order(sequence: u64) -> OrderWire {
    OrderWire {
        asset: Asset::perp(4),
        side: Side::Buy,
        px: Px::parse(b"1670.1").unwrap(),
        sz: Sz::parse(b"0.0147").unwrap(),
        reduce_only: false,
        kind: OrderKind::Limit(Tif::Gtc),
        cloid: Some(Cloid::from_parts(1, sequence)),
    }
}

fn main() {
    let wallet = Wallet::from_bytes(&[7; 32]).unwrap();
    let one = [order(1)];
    let batch: Vec<OrderWire> = (0..39).map(order).collect();
    let single = Action::Order { orders: &one, grouping: Grouping::Na, builder: None };
    let full = Action::Order { orders: &batch, grouping: Grouping::Na, builder: None };
    let options =
        L1Options { network: Network::Mainnet, nonce: 1, vault: None, expires_after_ms: Some(2) };
    let digest = keccak(b"digest");
    let signature = wallet.sign_digest(&digest);
    let mut target = vec![0u8; 64 * 1024];
    let mut tokens = vec![Token::ZERO; 8192];

    measure("decimal parse", 200_000, || {
        black_box(Px::parse(black_box(b"76029.5")).unwrap());
    });
    measure("decimal to wire", 200_000, || {
        black_box(black_box(Px::from_raw(7_602_950_000_000)).to_wire());
    });
    measure("action hash, 1 order", 50_000, || {
        black_box(l1_action_hash(black_box(&single), 1, None, Some(2)));
    });
    measure("action hash, 39 orders", 10_000, || {
        black_box(l1_action_hash(black_box(&full), 1, None, Some(2)));
    });
    measure("body JSON, 39 orders", 10_000, || {
        let mut sink = JsonSink::new(&mut target);
        full.walk(&mut sink);
        black_box(sink.finish().unwrap());
    });
    measure("recover", 2_000, || {
        black_box(recover(&digest, &signature));
    });
    measure("sign with recovery check", 2_000, || {
        black_box(wallet.sign_digest(black_box(&digest)));
    });
    measure("sign_l1, 1 order, all included", 2_000, || {
        black_box(sign_l1(black_box(&single), &options, &wallet));
    });
    measure("digest only", 50_000, || {
        black_box(l1_digest(black_box(&digest), Network::Mainnet));
    });
    measure("parse l2Book message", 50_000, || {
        black_box(parse(ORDER_BOOK.as_bytes(), &mut tokens).unwrap().root());
    });
    let mut book = Book::EMPTY;
    measure("parse and apply l2Book", 50_000, || {
        let document = parse(ORDER_BOOK.as_bytes(), &mut tokens).unwrap();
        let data = document.root().get("data").unwrap();
        book = Book::EMPTY;
        black_box(book.apply(data).unwrap());
    });
    measure("parse allMids, 25 KB", 5_000, || {
        black_box(parse(ALL_MIDS.as_bytes(), &mut tokens).unwrap().root());
    });
    println!(
        "\nsize_of<Book> {} bytes, size_of<Token> {} bytes",
        size_of::<Book>(),
        size_of::<Token>()
    );
    println!("size_of<Engine<1024, 8>> {} bytes", size_of::<hypertiger::engine::Engine<1024, 8>>());
}
