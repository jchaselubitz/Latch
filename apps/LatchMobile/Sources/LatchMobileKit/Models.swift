import Foundation

/// One row of `GET /v1/sessions`.
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

/// `GET /v1/sessions`.
public struct ListReport: Decodable, Equatable, Sendable {
    public let sessions: [SessionSummary]

    public init(sessions: [SessionSummary]) {
        self.sessions = sessions
    }
}

/// Whether a harness connector can produce events for one session.
public struct EventsAvailability: Decodable, Equatable, Sendable {
    public let ok: Bool
    public let reason: String?
    public let harness: String?
    /// The epoch a stored cursor must match. Replaying a cursor from a
    /// different epoch would interleave two unrelated timelines.
    public let connectorEpoch: Int?

    public init(
        ok: Bool,
        reason: String? = nil,
        harness: String? = nil,
        connectorEpoch: Int? = nil
    ) {
        self.ok = ok
        self.reason = reason
        self.harness = harness
        self.connectorEpoch = connectorEpoch
    }
}

/// `GET /v1/sessions/{id}/capabilities`.
///
/// The interaction fields are flattened into this document on the wire, so it
/// decodes them from the same container rather than a nested key.
public struct SessionCapabilities: Decodable, Equatable, Sendable {
    public let interaction: InteractionCapabilities
    public let events: EventsAvailability

    public init(interaction: InteractionCapabilities, events: EventsAvailability) {
        self.interaction = interaction
        self.events = events
    }

    private enum CodingKeys: String, CodingKey {
        case events
    }

    public init(from decoder: Decoder) throws {
        interaction = try InteractionCapabilities(from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        events = try container.decodeIfPresent(EventsAvailability.self, forKey: .events)
            ?? EventsAvailability(ok: false, reason: "the gateway did not report events")
    }
}

/// One `POST /v1/sessions/{id}/send` operation.
public enum SendOperation: Equatable, Sendable {
    case message(String)
    case keys(String)
    case resolve(requestId: String, choice: String)

    /// The contract's operation name.
    public var name: String {
        switch self {
        case .message: return "message"
        case .keys: return "keys"
        case .resolve: return "resolve"
        }
    }

    /// Whether a retry of this operation is safe to deduplicate.
    ///
    /// `keys` is intentionally non-idempotent: raw keypresses have no
    /// request identity, and the gateway rejects a key sent with one.
    public var acceptsIdempotencyKey: Bool {
        switch self {
        case .message, .resolve: return true
        case .keys: return false
        }
    }

    /// The request body, matching `send-request.schema.json`.
    public var body: [String: JSONValue] {
        switch self {
        case .message(let text):
            return ["message": .string(text)]
        case .keys(let keys):
            return ["keys": .string(keys)]
        case .resolve(let requestId, let choice):
            return [
                "resolve": .object([
                    "requestId": .string(requestId),
                    "choice": .string(choice)
                ])
            ]
        }
    }
}

/// The gateway's confirmation for one send.
public struct SendReport: Decodable, Equatable, Sendable {
    public let sessionId: String
    public let operation: String
    public let requestId: String?
    public let choice: String?
    /// Whether the operation reached the terminal.
    public let sent: Bool
    /// Whether a request-bound resolution was applied.
    public let resolved: Bool

    public init(
        sessionId: String,
        operation: String,
        requestId: String? = nil,
        choice: String? = nil,
        sent: Bool,
        resolved: Bool
    ) {
        self.sessionId = sessionId
        self.operation = operation
        self.requestId = requestId
        self.choice = choice
        self.sent = sent
        self.resolved = resolved
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
            return """
            This gateway speaks Latch protocol \(reported); this app implements \(supported). \
            Update the app or the `latch` CLI so both sides agree.
            """
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
