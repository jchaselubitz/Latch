//! Latch's headless session kernel.
//!
//! One `latchd` process contains one persistent terminal session: a child in
//! a PTY, a screen model kept off the live path, and a unix socket on which
//! clients attach (exclusively, by steal) or drive the session (concurrently,
//! by control verbs and events). There is no central server and no window,
//! tab, or pane model; presentation belongs to whoever attaches.
//!
//! Architecture and the decision behind it: `planning/HEADLESS_KERNEL_PROPOSAL.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod client;
pub mod daemon;
pub mod keys;
pub mod paths;
pub mod peer;
pub mod protocol;
pub mod pty;
pub mod render;

/// Name of the daemon binary shipped next to `latch`.
pub const BINARY_NAME: &str = "latchd";
