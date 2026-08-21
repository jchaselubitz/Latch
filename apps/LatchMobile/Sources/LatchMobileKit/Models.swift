import Foundation

/// One row of `GET /v2/sessions`.
///
/// The session list is the CLI's `latch list --json` report, which is
/// snake_case on the wire, unlike the camelCase discovery and interaction
/// documents. The two conventions are part of the contract, so this type
/// spells its keys out rather than applying a blanket key strategy.
public struct SessionSummary: Decodable, Equatable, Identifiable, Sendable {
    public let id: String
    public let name: String
    public let title: String?
    public let state: String
    public let cwd: String
    public let commandLabel: String
    public let createdAt: String
    public let lastActivityAt: String?
    public let idleMs: Int?

    private enum CodingKeys: String, CodingKey {
        case id
        case name
        case title
        case state
        case cwd
        case commandLabel = "command_label"
        case createdAt = "created_at"
        case lastActivityAt = "last_activity_at"
        case idleMs = "idle_ms"
    }

    public init(
        id: String,
        name: String,
        title: String? = nil,
        state: String,
        cwd: String,
        commandLabel: String,
        createdAt: String,
        lastActivityAt: String? = nil,
        idleMs: Int? = nil
    ) {
        self.id = id
        self.name = name
        self.title = title
        self.state = state
        self.cwd = cwd
        self.commandLabel = commandLabel
        self.createdAt = createdAt
        self.lastActivityAt = lastActivityAt
        self.idleMs = idleMs
    }

    /// What to show as the session's heading.
    public var displayName: String {
        if let title, !title.isEmpty { return title }
        return name
    }

    /// The last path component of `cwd`, which is how people recognize a
    /// session on a small screen.
    public var directoryName: String {
        URL(fileURLWithPath: cwd).lastPathComponent
    }

    /// Whether the session is still live enough to accept input.
    public var isRunning: Bool {
        state == "running" || state == "creating"
    }
}

/// `GET /v2/sessions`.
public struct ListReport: Decodable, Equatable, Sendable {
    public let sessions: [SessionSummary]

    public init(sessions: [SessionSummary]) {
        self.sessions = sessions
    }
}

/// Everything that can go wrong between the phone and a Latch gateway.
public enum LatchError: Error, Equatable, Sendable {
    /// The gateway URL is not a usable http(s) address.
    case invalidURL(String)
    /// The token was missing or rejected.
    case unauthorized
    /// The harness declined the operation, with the reason it gave. This is
    /// the authoritative answer; the `canSend` preflight is only a hint.
    case refused(String)
    /// Any other non-2xx response.
    case http(status: Int, path: String, reason: String)
    /// The response body did not match the contract.
    case malformedResponse(String)
    /// The gateway reports a protocol major this build does not implement.
    case unsupportedProtocol(reported: Int, supported: Int)
    /// The address is the Latch control plane, which has no session API.
    case notAGateway
    /// The feature was not advertised by discovery, so it must not be used.
    case endpointUnavailable(GatewayEndpointsName)
    /// The transport failed.
    case transport(String)

    /// The version disagreement behind this error, when that is what it is.
    ///
    /// Callers use this to render an actionable screen instead of a generic
    /// failure: a protocol mismatch is not a connection problem, and treating
    /// it as one costs the person a network-debugging detour.
    public var protocolMismatch: ProtocolMismatch? {
        guard case .unsupportedProtocol(let reported, let supported) = self else { return nil }
        return ProtocolMismatch(reported: reported, supported: supported)
    }

    public var message: String {
        switch self {
        case .invalidURL(let url):
            return "\(url) is not a valid gateway address."
        case .unauthorized:
            return "The gateway rejected this token. Check Settings, or run `latch serve token` again."
        case .refused(let reason):
            return reason
        case .http(let status, let path, let reason):
            return "\(path) failed (\(status)): \(reason)"
        case .malformedResponse(let detail):
            return "The gateway sent an unexpected response: \(detail)"
        case .unsupportedProtocol(let reported, let supported):
            // Name the side that can act. The old symmetric wording ("update
            // the app or the CLI") left the person to work out which half was
            // theirs, on a screen that had already told them the computer was
            // unreachable.
            return ProtocolMismatch(reported: reported, supported: supported).summary
        case .notAGateway:
            return """
            That's the Latch control plane, not your Mac. Pair this phone under Remote access \
            using the code your Mac shows, or enter a tunnel to `latch serve` — not the \
            control-plane URL from Mac Remote Access settings.
            """
        case .endpointUnavailable(let endpoint):
            return "This gateway does not offer \(endpoint.rawValue)."
        case .transport(let detail):
            return detail
        }
    }
}
