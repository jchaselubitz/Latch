//! The CLI: create, attach, list, and the rest of the command surface.
//!
//! The binary's clap surface in `main.rs` is the public contract; this module is
//! the behaviour behind it. Tests exercise both: the binary for end-to-end
//! behaviour, and these APIs for the pieces that deserve to be asserted without
//! a terminal — nesting policy, raw-mode restoration, JSON schemas, list sort
//! order.

pub mod attach;
pub mod create;
pub mod json;
pub mod manage;
pub mod nesting;
pub mod open;
pub mod serve;
pub mod term;
pub mod update;

pub use attach::{AttachOptions, RetryPolicy};
pub use create::{CreateOptions, CreateOutcome};
pub use json::{
    CapabilitiesReport, ConfigReport, DoctorReport, InspectReport, ListReport, OpenReport,
    PruneReport, RemoveReport, RenameReport, ResizeReport, SessionSummary, StopReport,
    UpdateReport,
};
pub use manage::{
    capabilities, config, doctor, inspect, list, prune, remove, rename, resize, stop,
    ConfigRequest, DoctorOptions, InspectOptions, ListOptions, PruneOptions, RemoveRequest,
    RenameRequest, ResizeRequest, StopRequest,
};
pub use nesting::{nesting_decision, NestingDecision, SESSION_ID_ENV};
pub use open::{open, OpenRequest};
pub use term::RawModeGuard;
pub use update::{update, UpdateOptions, UpdateStatus};
