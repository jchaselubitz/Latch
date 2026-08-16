//! Policy shared by the Mac helper and iOS binding.

use async_trait::async_trait;
use thiserror::Error;

/// The path selected by ICE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedPath {
    /// A host or server-reflexive candidate pair.
    Direct,
    /// A TURN-relayed candidate pair.
    Relay,
}

/// A server made available to the ICE agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IceServer {
    /// STUN/TURN URL without credentials embedded in it.
    pub url: String,
    /// TURN username, empty for STUN.
    pub username: String,
    /// TURN credential, empty for STUN.
    pub credential: String,
}

impl IceServer {
    /// Returns true when this URL can allocate a relayed candidate.
    pub fn is_turn(&self) -> bool {
        let scheme = self.url.split(':').next().unwrap_or_default();
        scheme.eq_ignore_ascii_case("turn") || scheme.eq_ignore_ascii_case("turns")
    }
}

/// Why the shared policy refused or lost a connection.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TransportPolicyError {
    /// TURN was exposed to the first attempt, which would allow an unrecorded relay.
    #[error("TURN credentials are withheld until a direct attempt fails")]
    TurnBeforeDirectFailure,
    /// Relay retry was requested without a prior failed direct attempt.
    #[error("relay retry requires a recorded direct failure")]
    RelayWithoutDirectFailure,
    /// The credential response contained no TURN service.
    #[error("relay retry requires at least one TURN server")]
    MissingTurnServer,
    /// Noise proved a key other than the pairing record's pinned key.
    #[error("the remote Noise identity does not match the paired Mac")]
    PeerIdentityMismatch,
    /// The underlying transport failed.
    #[error("remote transport failed: {0}")]
    Backend(String),
}

/// Result of opening the reliable ordered data channel.
#[derive(Debug)]
pub struct Connected<C> {
    /// Channel carrying length-bounded Noise records.
    pub channel: C,
    /// ICE path selected for this connection.
    pub path: SelectedPath,
}

/// Backend seam used by the policy and deterministic tests.
#[async_trait]
pub trait ConnectionBackend: Send {
    /// The reliable ordered channel returned by this backend.
    type Channel: Send;

    /// Connect with exactly the supplied ICE servers.
    async fn connect(
        &mut self,
        servers: &[IceServer],
    ) -> Result<Connected<Self::Channel>, TransportPolicyError>;
}

/// Direct-first coordinator.
///
/// The coordinator enforces policy by withholding TURN URLs from the first
/// agent. A relay-capable retry is impossible until that attempt has failed.
pub struct DirectFirst<B> {
    backend: B,
    direct_failed: bool,
    selected_path: Option<SelectedPath>,
    rediscovery_required: bool,
}

impl<B> DirectFirst<B>
where
    B: ConnectionBackend,
{
    /// Creates a fresh coordinator.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            direct_failed: false,
            selected_path: None,
            rediscovery_required: false,
        }
    }

    /// Attempts a direct connection with STUN only.
    pub async fn connect_direct(
        &mut self,
        servers: &[IceServer],
    ) -> Result<Connected<B::Channel>, TransportPolicyError> {
        if servers.iter().any(IceServer::is_turn) {
            return Err(TransportPolicyError::TurnBeforeDirectFailure);
        }
        match self.backend.connect(servers).await {
            Ok(connected) => {
                self.record_path(connected.path);
                Ok(connected)
            }
            Err(error) => {
                self.direct_failed = true;
                Err(error)
            }
        }
    }

    /// Retries with TURN after a recorded direct failure.
    pub async fn retry_with_relay(
        &mut self,
        servers: &[IceServer],
    ) -> Result<Connected<B::Channel>, TransportPolicyError> {
        if !self.direct_failed {
            return Err(TransportPolicyError::RelayWithoutDirectFailure);
        }
        if !servers.iter().any(IceServer::is_turn) {
            return Err(TransportPolicyError::MissingTurnServer);
        }
        let connected = self.backend.connect(servers).await?;
        self.record_path(connected.path);
        Ok(connected)
    }

    /// Records a nominated replacement pair (for relay-to-direct recovery).
    pub fn path_changed(&mut self, path: SelectedPath) {
        self.record_path(path);
    }

    /// Returns whether gateway capabilities must be re-discovered before use.
    pub fn take_rediscovery_required(&mut self) -> bool {
        std::mem::take(&mut self.rediscovery_required)
    }

    fn record_path(&mut self, path: SelectedPath) {
        if self
            .selected_path
            .replace(path)
            .is_some_and(|old| old != path)
        {
            self.rediscovery_required = true;
        }
    }
}

/// Verifies the static key proven by Noise against the pairing record's pin.
///
/// DTLS certificates and control-plane identity fields are intentionally not
/// inputs: neither is an authorization identity in Latch.
pub fn verify_noise_peer(
    proven_static_key: &[u8],
    pinned_static_key: &[u8],
) -> Result<(), TransportPolicyError> {
    let equal = proven_static_key.len() == pinned_static_key.len()
        && proven_static_key
            .iter()
            .zip(pinned_static_key)
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0;
    equal
        .then_some(())
        .ok_or(TransportPolicyError::PeerIdentityMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeBackend {
        results: VecDeque<Result<SelectedPath, TransportPolicyError>>,
        calls: Vec<Vec<IceServer>>,
    }

    #[async_trait]
    impl ConnectionBackend for FakeBackend {
        type Channel = ();

        async fn connect(
            &mut self,
            servers: &[IceServer],
        ) -> Result<Connected<Self::Channel>, TransportPolicyError> {
            self.calls.push(servers.to_vec());
            self.results
                .pop_front()
                .expect("test supplied a backend result")
                .map(|path| Connected { channel: (), path })
        }
    }

    fn stun() -> IceServer {
        IceServer {
            url: "stun:example.test:3478".into(),
            username: String::new(),
            credential: String::new(),
        }
    }

    fn turn() -> IceServer {
        IceServer {
            url: "turn:example.test:3478?transport=udp".into(),
            username: "device".into(),
            credential: "secret".into(),
        }
    }

    #[tokio::test]
    async fn turn_is_withheld_until_direct_fails() {
        let backend = FakeBackend {
            results: VecDeque::from([
                Err(TransportPolicyError::Backend("direct failed".into())),
                Ok(SelectedPath::Relay),
            ]),
            calls: vec![],
        };
        let mut policy = DirectFirst::new(backend);

        assert_eq!(
            policy.connect_direct(&[stun(), turn()]).await.unwrap_err(),
            TransportPolicyError::TurnBeforeDirectFailure
        );
        assert!(policy.connect_direct(&[stun()]).await.is_err());
        assert_eq!(
            policy
                .retry_with_relay(&[stun(), turn()])
                .await
                .unwrap()
                .path,
            SelectedPath::Relay
        );
        assert!(policy.backend.calls[0]
            .iter()
            .all(|server| !server.is_turn()));
        assert!(policy.backend.calls[1].iter().any(IceServer::is_turn));
    }

    #[tokio::test]
    async fn relay_without_direct_failure_is_refused_without_calling_backend() {
        let backend = FakeBackend {
            results: VecDeque::new(),
            calls: vec![],
        };
        let mut policy = DirectFirst::new(backend);
        assert_eq!(
            policy.retry_with_relay(&[turn()]).await.unwrap_err(),
            TransportPolicyError::RelayWithoutDirectFailure
        );
        assert!(policy.backend.calls.is_empty());
    }

    #[test]
    fn noise_identity_mismatch_is_terminal() {
        assert_eq!(
            verify_noise_peer(&[1; 32], &[2; 32]),
            Err(TransportPolicyError::PeerIdentityMismatch)
        );
        assert_eq!(verify_noise_peer(&[1; 32], &[1; 32]), Ok(()));
    }

    #[test]
    fn path_change_requires_capability_rediscovery_once() {
        let backend = FakeBackend {
            results: VecDeque::new(),
            calls: vec![],
        };
        let mut policy = DirectFirst::new(backend);
        policy.path_changed(SelectedPath::Relay);
        assert!(!policy.take_rediscovery_required());
        policy.path_changed(SelectedPath::Direct);
        assert!(policy.take_rediscovery_required());
        assert!(!policy.take_rediscovery_required());
    }
}
