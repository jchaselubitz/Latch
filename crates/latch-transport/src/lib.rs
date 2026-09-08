//! Shared remote transport for the dedicated Mac helper and the iOS XCFramework.
//!
//! The crate owns the reusable authenticated link: system-validated WSS or
//! framed LAN records, Noise XX peer authentication, and bounded Yamux streams.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// Remote-link v1 contracts shared by WSS, LAN, the helper, and UniFFI.
pub mod link;

/// Diagnostic name emitted by the dedicated helper.
pub const STACK_NAME: &str = "wss-or-lan/noise-xx/yamux/v1";
