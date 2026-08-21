//! Latch's CLI, private tmux engine, and durable metadata contracts.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod cli;
/// Agent-neutral conversation domain and connector boundary.
// The domain, connector trait, and Hub API are complete ahead of the Claude and
// Codex adapters, which are the callers for the observation and reconciliation
// halves. Narrowing them to today's callers would only have to be undone.
#[allow(dead_code)]
pub(crate) mod conversation;
pub mod engine;
pub mod observer;
pub mod session;
