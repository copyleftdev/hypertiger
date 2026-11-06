//! The VOPR: a deterministic simulator that puts the order engine through bad worlds.
//!
//! One seed fixes everything: the faults enabled, every delay, every lost reply, every fill. A
//! failure is a seed, and a seed replays exactly.
#![forbid(unsafe_code)]
// A test harness: an `expect` here is an assertion about the simulator itself, and allocation is
// fine, since the simulator is not the thing being held to static allocation.
#![allow(clippy::expect_used, clippy::unwrap_used)]

pub mod model;
pub mod prng;
pub mod world;
