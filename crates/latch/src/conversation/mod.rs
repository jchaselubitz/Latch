//! The Conversation Hub's agent-neutral domain surface.
//!
//! Connectors only emit [`ConnectorMutation`] values.  The projection stamps
//! ordinals, revisions, and generations so no agent-specific source can affect
//! ordering or wire-level identity.

mod cache;
mod connector;
mod connectors;
mod hub;
mod model;
mod pending;
mod projection;

pub use cache::*;
pub use connector::*;
pub use connectors::*;
pub use hub::*;
pub use model::*;
pub use pending::*;
pub use projection::*;
