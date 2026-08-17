import Foundation

/// The client half of the `/v1` compatibility boundary.
///
/// `docs/REMOTE_SDK.md` fixes these rules and the TypeScript SDK implements the
/// same ones. They exist so this app keeps working against a newer gateway and
/// degrades honestly against an older one:
///
/// - `/v1` is a compatibility boundary, not one server release. Additions are
///   optional and additive.
/// - `GET /v1/capabilities` is the mandatory discovery step. An optional
///   endpoint may be used only when the map reports it as `true`.
/// - A client must never probe an optional endpoint and infer support from the
///   error it gets back.
/// - A gateway that 404s discovery is the pre-discovery, terminal-only build,
///   unless the body is the control plane's unmatched-route contract.
/// - A `protocolVersion` other than the supported major is unsupported rather
///   than guessed at.
public enum GatewayCompatibility {
    /// The control plane's unmatched-route body.
    ///
    /// That service has pairing and signaling routes, not a session gateway.
    /// A 404 on `/v1/capabilities` is therefore not the pre-discovery
    /// `latch serve` surface: treating it as one makes the phone look linked
    /// and then fail `/v1/sessions` with the same 404.
    public static func isControlPlaneUnmatchedRoute(
        status: Int,
        code: String?,
        reason: String
    ) -> Bool {
        status == 404 && code == "not_found" && reason == "no such resource"
    }

    /// What a gateway with no `/v1/capabilities` endpoint can do.
    ///
    /// Sessions and terminal predate discovery, so they are safe to assume.
    /// Everything introduced alongside discovery is off: without the document
    /// there is no way to learn about it that is not a probe.
    public static func legacyCapabilities() -> GatewayCapabilities {
        GatewayCapabilities(
            protocolVersion: LatchContract.protocolVersion,
            productVersion: "",
            endpoints: GatewayEndpoints(
                sessions: true,
                sessionCapabilities: false,
                terminal: true,
                events: false,
                send: false
            ),
            features: GatewayFeatures(),
            gatewayInstanceId: ""
        )
    }

    /// Whether this build may call `endpoint` on the discovered gateway.
    ///
    /// An unsupported protocol major disables everything: field meanings are
    /// only guaranteed within one major, so "the map said true" carries no
    /// weight once the major disagrees.
    public static func supports(
        endpoint: GatewayEndpointsName,
        capabilities: GatewayCapabilities?
    ) -> Bool {
        guard let capabilities,
              capabilities.protocolVersion == LatchContract.protocolVersion
        else { return false }
        return capabilities.endpoints.isEnabled(endpoint)
    }

    /// Whether this build may rely on an optional feature.
    public static func supports(
        feature: GatewayFeaturesName,
        capabilities: GatewayCapabilities?
    ) -> Bool {
        guard let capabilities,
              capabilities.protocolVersion == LatchContract.protocolVersion
        else { return false }
        return capabilities.features.isEnabled(feature)
    }

    /// Rejects a gateway whose protocol major this build does not implement.
    public static func validate(_ capabilities: GatewayCapabilities) throws {
        guard capabilities.protocolVersion == LatchContract.protocolVersion else {
            throw LatchError.unsupportedProtocol(
                reported: capabilities.protocolVersion,
                supported: LatchContract.protocolVersion
            )
        }
    }

    /// How the session screen should behave, given discovery.
    ///
    /// A gateway without `events` cannot back a chat transcript, and one
    /// without `send` cannot accept a message. The UI reads this instead of
    /// finding out by failing a request.
    public static func sessionSurface(for capabilities: GatewayCapabilities?) -> SessionSurface {
        let events = supports(endpoint: .events, capabilities: capabilities)
        let send = supports(endpoint: .send, capabilities: capabilities)
        let interaction = supports(endpoint: .sessionCapabilities, capabilities: capabilities)
        if !events {
            return SessionSurface(chat: false, composer: false, interactionControls: false)
        }
        return SessionSurface(chat: true, composer: send, interactionControls: interaction)
    }
}

/// Which parts of the session screen discovery permits.
public struct SessionSurface: Equatable, Sendable {
    /// Show the transcript. Without it the session is terminal-only, which
    /// this first app release does not implement.
    public let chat: Bool
    /// Offer the message composer.
    public let composer: Bool
    /// Offer request-bound controls, such as answering a permission prompt.
    public let interactionControls: Bool

    public init(chat: Bool, composer: Bool, interactionControls: Bool) {
        self.chat = chat
        self.composer = composer
        self.interactionControls = interactionControls
    }

    /// Applies the grant attached to a paired route after discovery has
    /// determined which controls the gateway implements. The Mac remains the
    /// authority for every operation, but an observe-only phone must not
    /// present a composer it cannot use.
    public func restricted(to permission: DevicePermission?) -> SessionSurface {
        guard let permission, !permission.permits(.interact) else { return self }
        return SessionSurface(chat: chat, composer: false, interactionControls: false)
    }
}
