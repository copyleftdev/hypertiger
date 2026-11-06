//! The edge: real sockets, real TLS and a real clock, around the sans-IO kernel.
//!
//! This is the only crate in the workspace that touches the operating system, so it is the only one
//! the simulator cannot see. It holds as little logic as it can. Sockets are non-blocking and
//! polled on the caller's tick, in keeping with TigerStyle's rule that a program runs at its own
//! pace, and there is no async runtime: `Session::step` does a bounded amount of work and returns.
//!
//! TLS allocates inside `rustls`, so the zero-allocation guarantee stops at this crate's boundary.
#![forbid(unsafe_code)]

pub mod conn;
pub mod session;

pub use conn::{Connection, NetError, Stream, WsClient, WsEvent};
pub use session::{Clock, Connector, Session, SessionConfig, SystemClock, TlsConnector};

/// A network's two hosts. Which one to use is always the caller's explicit choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    pub name: &'static str,
}

pub const HOST_MAINNET: Host = Host { name: "api.hyperliquid.xyz" };
pub const HOST_TESTNET: Host = Host { name: "api.hyperliquid-testnet.xyz" };

/// Wall-clock milliseconds, which is what the exchange's nonces and expiries are measured in.
#[must_use]
pub fn now_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        // Before 1970, which is a broken clock. Time here only moves forward from what it was.
        Err(_) => 0,
    }
}

/// Eight unpredictable bytes from the standard library's per-process random hash seed. It is used
/// for WebSocket masks and handshake keys, where unpredictability prevents proxy confusion and
/// nothing is secret: TLS is what protects the traffic.
#[must_use]
pub fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}
