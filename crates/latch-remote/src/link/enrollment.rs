//! Provisional, QR-bound enrollment: the controller's proposal, the owner's
//! decision on the Mac, and the receipt written back to the phone.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use latch::cli::remote_access::{
    authorize_enrollment, remote_link_identity, validate_device_name, DevicePermission,
};
use latch::session::paths::LatchHome;
use latch_transport::link::{
    LinkConfig, LinkPurpose, LinkRole, LinkTimings, SecureLink, Service, WssRecordIo,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use super::config::decode_key;
use super::ipc::{emit_json, emit_status, ipc_reader, HostStatus};
use super::RemoteLinkHostConfig;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentProposal {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    name: String,
    permission: DevicePermission,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentDecision {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    permission: DevicePermission,
    approved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentMirrored {
    r#type: String,
    version: u8,
    enrollment_id: String,
    provisional_device_id: String,
    controller_public_key: String,
    permission: DevicePermission,
    grant_revision: u64,
}

pub(super) async fn run_enrollment(
    home: LatchHome,
    config: RemoteLinkHostConfig,
) -> anyhow::Result<()> {
    let mut ipc = ipc_reader("latch-remote-enrollment-ipc")?;
    let identity = remote_link_identity(&home)?;
    let local_private = decode_key(&identity.private_key)?;
    let local_public = decode_key(&identity.public_key)?;
    let enrollment_id = config.enrollment_id.as_deref().expect("validated");
    let enrollment_secret = decode_key(config.enrollment_secret.as_deref().expect("validated"))?;
    let (mut records, _control) =
        WssRecordIo::connect_with_control(&config.relay_url, &config.admission)
            .await
            .context("cannot connect enrollment relay")?;
    emit_status(HostStatus::WaitingForPeer, serde_json::json!({}));
    // The owner is holding a QR code up to a phone; the provisional room lives
    // five minutes and so does this wait.
    records
        .wait_for_peer(Some(Duration::from_secs(5 * 60)))
        .await
        .context("no phone joined the enrollment room")?;
    emit_status(HostStatus::Authenticating, serde_json::json!({}));
    let link = SecureLink::establish(
        records,
        LinkConfig {
            purpose: LinkPurpose::Enrollment,
            role: LinkRole::Host,
            local_private_key: Zeroizing::new(local_private),
            local_public_key: local_public.clone(),
            expected_remote_public_key: None,
            enrollment_id: Some(enrollment_id.to_owned()),
            enrollment_secret: Some(Zeroizing::new(enrollment_secret)),
            grant_revision: 0,
            timings: LinkTimings::default(),
        },
    )
    .await
    .context("enrollment authentication failed")?;
    let result = async {
        let (header, mut stream) = link.accept(LinkPurpose::Enrollment).await?;
        if header.service != Service::Enrollment || header.grant_revision != 0 {
            bail!("controller requested a forbidden enrollment service");
        }
        let proposal: EnrollmentProposal = read_json_line(&mut stream).await?;
        validate_proposal(&proposal, enrollment_id, &link.remote_public_key)?;
        let controller_key = decode_key(&proposal.controller_public_key)?;
        let permission = permission_name(proposal.permission);
        let comparison = link.enrollment_comparison(enrollment_id, &controller_key, permission)?;
        emit_json(&serde_json::json!({
            "type": "enrollment_pending",
            "version": 1,
            "enrollmentId": enrollment_id,
            "provisionalDeviceId": proposal.provisional_device_id,
            "controllerPublicKey": proposal.controller_public_key,
            "name": proposal.name,
            "permission": proposal.permission,
            "comparison": comparison,
        }))?;

        let command = ipc
            .recv()
            .await
            .ok_or_else(|| anyhow!("Desktop IPC closed before enrollment decision"))?;
        let decision: EnrollmentDecision =
            serde_json::from_str(&command).context("invalid enrollment decision")?;
        validate_decision(&decision, &proposal)?;
        if !decision.approved {
            emit_json(&serde_json::json!({
                "type": "enrollment_rejected", "version": 1,
                "enrollmentId": enrollment_id,
            }))?;
            return Ok(());
        }

        let committed = authorize_enrollment(
            &home,
            enrollment_id,
            &proposal.controller_public_key,
            &proposal.name,
            proposal.permission,
            &proposal.provisional_device_id,
        )?;
        emit_json(&serde_json::json!({
            "type": "enrollment_committed", "version": 1,
            "enrollmentId": enrollment_id,
            "provisionalDeviceId": proposal.provisional_device_id,
            "controllerPublicKey": proposal.controller_public_key,
            "permission": proposal.permission,
            "grantRevision": committed.grant_revision,
        }))?;

        let command = ipc
            .recv()
            .await
            .ok_or_else(|| anyhow!("Desktop IPC closed before enrollment mirror confirmation"))?;
        let mirrored: EnrollmentMirrored =
            serde_json::from_str(&command).context("invalid enrollment mirror confirmation")?;
        validate_mirrored(&mirrored, &proposal, committed.grant_revision)?;
        write_json_line(
            &mut stream,
            &serde_json::json!({
                "type": "pairing_approved",
                "version": 1,
                "enrollmentId": enrollment_id,
                "hostPublicKey": identity.public_key,
                "controllerPublicKey": proposal.controller_public_key,
                "permission": proposal.permission,
                "grantRevision": committed.grant_revision,
            }),
        )
        .await?;
        stream.shutdown().await?;
        emit_json(&serde_json::json!({
            "type": "enrollment_complete", "version": 1,
            "enrollmentId": enrollment_id,
        }))?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    link.close().await;
    result
}

fn validate_proposal(
    proposal: &EnrollmentProposal,
    enrollment_id: &str,
    authenticated_key: &[u8],
) -> anyhow::Result<()> {
    if proposal.r#type != "enrollment_proposal"
        || proposal.version != 1
        || proposal.enrollment_id != enrollment_id
        || proposal.provisional_device_id.is_empty()
        || proposal.provisional_device_id.len() > 96
        || decode_key(&proposal.controller_public_key)? != authenticated_key
    {
        bail!("enrollment proposal does not match the authenticated controller");
    }
    // Checked before the proposal is shown to the owner; `authorize_enrollment`
    // checks again before anything is stored.
    validate_device_name(&proposal.name)
        .context("enrollment proposal has an invalid device name")?;
    Ok(())
}

fn validate_decision(
    decision: &EnrollmentDecision,
    proposal: &EnrollmentProposal,
) -> anyhow::Result<()> {
    if decision.r#type != "enrollment_decision"
        || decision.version != 1
        || decision.enrollment_id != proposal.enrollment_id
        || decision.provisional_device_id != proposal.provisional_device_id
        || decision.controller_public_key != proposal.controller_public_key
        || decision.permission != proposal.permission
    {
        bail!("enrollment decision does not match the pending proposal");
    }
    Ok(())
}

fn validate_mirrored(
    mirrored: &EnrollmentMirrored,
    proposal: &EnrollmentProposal,
    grant_revision: u64,
) -> anyhow::Result<()> {
    if mirrored.r#type != "enrollment_mirrored"
        || mirrored.version != 1
        || mirrored.enrollment_id != proposal.enrollment_id
        || mirrored.provisional_device_id != proposal.provisional_device_id
        || mirrored.controller_public_key != proposal.controller_public_key
        || mirrored.permission != proposal.permission
        || mirrored.grant_revision != grant_revision
    {
        bail!("enrollment mirror does not match the committed proposal");
    }
    Ok(())
}

fn permission_name(permission: DevicePermission) -> &'static str {
    match permission {
        DevicePermission::Observe => "observe",
        DevicePermission::Interact => "interact",
        DevicePermission::Control => "control",
    }
}

async fn read_json_line<T: DeserializeOwned>(
    stream: &mut latch_transport::link::LogicalStream,
) -> anyhow::Result<T> {
    const MAX_MESSAGE_BYTES: usize = 16 * 1024;
    let mut message = Vec::new();
    loop {
        let byte = stream
            .read_u8()
            .await
            .context("cannot read enrollment message")?;
        if byte == b'\n' {
            break;
        }
        if message.len() == MAX_MESSAGE_BYTES {
            bail!("enrollment message exceeds limit");
        }
        message.push(byte);
    }
    if message.is_empty() {
        bail!("empty enrollment message");
    }
    serde_json::from_slice(&message).context("invalid enrollment message")
}

async fn write_json_line(
    stream: &mut latch_transport::link::LogicalStream,
    value: &impl Serialize,
) -> anyhow::Result<()> {
    let mut message = serde_json::to_vec(value)?;
    if message.len() > 16 * 1024 {
        bail!("enrollment message exceeds limit");
    }
    message.push(b'\n');
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENROLLMENT: &str = "enr_fixture_00000001";

    fn proposal(name: &str) -> EnrollmentProposal {
        EnrollmentProposal {
            r#type: "enrollment_proposal".into(),
            version: 1,
            enrollment_id: ENROLLMENT.into(),
            provisional_device_id: "dev_fixture_00000001".into(),
            controller_public_key: "22".repeat(32),
            name: name.into(),
            permission: DevicePermission::Control,
        }
    }

    fn validate(name: &str) -> anyhow::Result<()> {
        validate_proposal(&proposal(name), ENROLLMENT, &[0x22; 32])
    }

    #[test]
    fn a_proposal_name_that_could_restyle_the_approval_prompt_is_rejected() {
        // The Mac approval prompt shows this name next to the requested grant;
        // a peer that controls line breaks or invisible characters in it could
        // make the prompt lead with a different grant than the one committed.
        for name in [
            "Jake's iPhone\nrequests Observe",
            "Jake\u{200d}'s iPhone",
            "iPhone requests Observe.\u{0}\u{b}Confirm iPhone",
            "iPhone\u{202e}requests Observe",
            "Jake's iPhone\trequests Observe",
            &"a".repeat(81),
        ] {
            assert!(validate(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn an_ordinary_device_name_is_accepted() {
        validate("Jake's iPhone (work)").unwrap();
        validate(&"a".repeat(80)).unwrap();
    }
}
