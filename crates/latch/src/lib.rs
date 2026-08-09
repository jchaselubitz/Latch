//! `latch` as a library: the session worker, exposed so it can be tested.
//!
//! The product is one binary (decision D1). This library target exists because
//! the worker is a state machine wrapped in POSIX process management, and those
//! two halves want different kinds of test. The state machine — attachment
//! registry, resize authority, bounded client queues — is exercised directly and
//! deterministically. Detachment, signals, socket lifetime, and hostile input
//! are exercised against the real binary through `CARGO_BIN_EXE_latch`, because
//! nothing else proves them.
//!
//! # Unsafe
//!
//! The leaf crates forbid `unsafe`; this one cannot. `setsid`, PTY allocation,
//! `killpg`, and peer-credential reads have no safe std equivalent, and wrapping
//! them is the reason this crate owns the process plane. Unsafe is confined to
//! [`worker::process`], [`worker::spawn`], and [`worker::socket`]; every other
//! module is ordinary safe Rust.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod cli;
pub mod worker;
