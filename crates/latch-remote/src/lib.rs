//! Internet-facing transport for the dedicated remote-access helper.
//!
//! The `latch` crate owns local authorization and gateway semantics. This
//! crate exclusively owns WSS/LAN connectivity, Noise, and multiplexing.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// WSS/Noise/Yamux host owner and authenticated gateway handoff.
pub mod link;
