//! Local authority and fixed loopback gateway for Remote Link.
//!
//! Internet and LAN transport live exclusively in `latch-transport`, driven
//! by `latch-remote`. This ordinary CLI crate stores the Mac identity and
//! exact controller grants, and accepts only already-authenticated logical
//! streams for proxying to a supervised loopback gateway.

mod devices;
mod framing;
mod gateway;

pub use crate::cli::serve::routes::Grant as DevicePermission;
pub use devices::{
    authorize_enrollment, grant, list_devices, read_audit, record_link_status,
    remote_link_identity, revoke, rotate_device_key, set_enabled, status, validate_device_name,
    DeviceSummary, RemoteAccessStatus, RemoteLinkIdentityMaterial, DEVICE_NAME_PUNCTUATION,
    MAX_DEVICE_NAME_BYTES,
};
#[cfg(feature = "remote-link-test")]
pub use gateway::proxy_authenticated_stream_for_test;
pub use gateway::{AuthenticatedGateway, SharedGatewayOwner, PROXY_IDLE_TIMEOUT};
