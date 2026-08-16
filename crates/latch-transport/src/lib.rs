//! Shared remote transport for the dedicated Mac helper and the iOS XCFramework.
//!
//! The crate owns connectivity (ICE, DTLS, SCTP, and one reliable ordered data
//! channel). Noise remains above this layer and is the only peer-authentication
//! mechanism used by Latch.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// Direct-first connection policy and transport-neutral byte-channel contract.
pub mod policy;
/// webrtc-rs ICE/DTLS/SCTP/data-channel implementation.
pub mod rtc;

/// Diagnostic name emitted by the dedicated helper.
pub const STACK_NAME: &str = "webrtc-ice/dtls/sctp/data+noise";
