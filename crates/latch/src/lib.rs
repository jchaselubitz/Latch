//! Latch's CLI, latchd engine, and durable metadata contracts.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod cli;
/// Agent-neutral conversation domain and connector boundary.
pub(crate) mod conversation;
pub mod engine;
mod hex;
pub mod observer;
pub mod session;
mod util;
