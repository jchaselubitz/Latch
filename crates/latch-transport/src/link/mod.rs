//! Authenticated, multiplexed Remote Link v1.
//!
//! - [`config`]: fixed bounds, roles, purposes, and the wire hello/header shapes.
//! - [`error`]: the fail-closed [`LinkError`].
//! - [`wss`] and [`lan`]: the two [`RecordIo`] carriers.
//! - [`handshake`]: Noise XX, the bound prologue, and LinkHello.
//! - [`secure`]: [`SecureLink`], its Yamux driver, and the encrypted transport loop.
//! - [`stream`]: [`LogicalStream`] and the per-stream service header.

use async_trait::async_trait;

mod config;
mod error;
mod handshake;
mod lan;
mod secure;
mod stream;
mod wss;

#[cfg(test)]
mod tests;

pub use config::{
    LinkConfig, LinkHello, LinkLimits, LinkPurpose, LinkRole, LinkStageTimings, LinkTimings,
    OpenService, Service, DEAD_PEER_TIMEOUT, HANDSHAKE_TIMEOUT, KEEPALIVE_INTERVAL,
    MAX_PENDING_RECORDS, MAX_RECEIVE_WINDOW_BYTES, MAX_RECORD_BYTES, MAX_STREAMS,
    RELAY_CONNECT_TIMEOUT, RELAY_INACTIVITY_TIMEOUT, RELAY_WRITE_TIMEOUT, WRITE_QUANTUM_BYTES,
};
pub use error::LinkError;
pub use lan::LanRecordIo;
pub use secure::SecureLink;
pub use stream::LogicalStream;
pub use wss::{RelayStatus, WssControl, WssRecordIo};

/// A record-preserving carrier. WSS uses one binary message per record; LAN
/// uses one bounded length-prefixed record.
#[async_trait]
pub trait RecordIo: Send + Unpin + 'static {
    /// Sends exactly one record.
    async fn send_record(&mut self, record: Vec<u8>) -> Result<(), LinkError>;
    /// Receives exactly one record, or `None` after normal closure.
    async fn recv_record(&mut self) -> Result<Option<Vec<u8>>, LinkError>;
    /// Closes the carrier.
    async fn close(&mut self) -> Result<(), LinkError>;
}
