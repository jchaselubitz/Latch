//! The CLI: create, attach, list, and the rest of the command surface.
//!
//! The binary's clap surface in `main.rs` is the public contract; this module is
//! the behaviour behind it. Tests exercise both: the binary for end-to-end
//! behaviour, and these APIs for the pieces that deserve to be asserted without
//! a terminal — nesting policy, JSON schemas, list sort
//! order.

pub mod agent_models;
pub mod attach;
pub mod commit_message;
pub mod create;
pub mod json;
pub mod manage;
pub mod nesting;
pub mod open;
pub mod remote_access;
pub mod serve;
pub mod term;
pub mod update;
