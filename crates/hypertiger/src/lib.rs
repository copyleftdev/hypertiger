//! The hypertiger kernel. See `docs/DESIGN.md` for what it guarantees and why.
//!
//! `no_std` with no `alloc` crate is the static-allocation rule enforced by the compiler: this
//! crate has no way to reach a heap, so no path through it can allocate after startup, or at all.
#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

// Lengths are u32 and u64 wherever they are stored or counted; `usize` appears only at a slice
// boundary. This assertion is what makes those conversions lossless.
const _: () = assert!(usize::BITS == 64);

pub mod action;
pub mod book;
pub mod decimal;
pub mod engine;
pub mod feed;
pub mod http;
pub mod info;
pub mod json;
pub mod limits;
pub mod market;
pub mod nonce;
pub mod signing;
pub mod sink;
pub mod types;
pub mod ws;
