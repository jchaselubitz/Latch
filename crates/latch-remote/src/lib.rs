//! Internet-facing transport for the dedicated remote-access helper.
//!
//! The `latch` crate owns the authenticated Noise proxy and every
//! authorization decision above it. This crate owns the connectivity beneath
//! it, and is the only place the ICE/DTLS/SCTP stack is linked.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// The helper's ICE responder, driven by rendezvous offers.
pub mod ice;
