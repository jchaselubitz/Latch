import Foundation

/// The address and bearer token for one `latch serve` gateway.
public struct GatewayLink: Equatable, Sendable, Codable {
    /// Base URL, without a trailing slash or a path.
    public let url: URL
    public let token: String

    public init(url: URL, token: String) {
        self.url = url
        self.token = token
    }

    /// Builds a link from what a person typed in Settings.
    ///
    /// `latch serve token` prints a bare token and the gateway is usually
    /// reached through a tunnel, so the address is the only part with room for
    /// a typo worth catching early.
    public init(address: String, token: String) throws {
        let trimmed = address.trimmingCharacters(in: .whitespacesAndNewlines)
        var text = trimmed
        while text.hasSuffix("/") { text.removeLast() }
        guard
            let url = URL(string: text),
            let scheme = url.scheme?.lowercased(),
            scheme == "http" || scheme == "https",
            url.host != nil
        else {
            throw LatchError.invalidURL(trimmed)
        }
        self.url = url
        self.token = token.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// The HTTP half of the Latch remote client.
///
/// The gateway itself is plaintext and loopback-only, so in practice this
/// points at an SSH tunnel, a Tailscale address, or a TLS-terminating reverse
/// proxy. That is a deployment decision the app does not try to make; it only
/// refuses to build a link out of an address it cannot use.
public actor LatchGateway {
    private let link: GatewayLink
    // Retain the transport for as long as its gateway client exists. This is
    // significant for the Noise implementation: it owns the loopback
    // listener URLSession connects to, whereas the HTTPS transport is only a
    // value wrapper around a manually entered link.
    private let transport: any GatewayTransport
    private let session: URLSession
    private let decoder = JSONDecoder()

    /// The discovery document, once fetched. Held so a request never has to
    /// probe an endpoint to find out whether it exists.
    private var capabilities: GatewayCapabilities?

    public init(link: GatewayLink, session: URLSession = .shared) {
        transport = HTTPSGatewayTransport(link: link)
        self.link = link
        self.session = session
    }

    /// Uses a transport-selected gateway link. `NoiseTunnelGatewayTransport`
    /// supplies a loopback URL and an empty token; the public HTTP contract is
    /// otherwise exactly the same as a manually configured gateway.
    public init(transport: any GatewayTransport, session: URLSession = .shared) {
        self.transport = transport
        link = transport.gatewayLink
        self.session = session
    }

    public var gateway: GatewayLink { link }

    /// The cached discovery document, if `discover()` has run.
    public var discovered: GatewayCapabilities? { capabilities }

    // MARK: - Discovery

    /// Runs the mandatory discovery step and caches the result.
    ///
    /// A 404 here is not a failure: it identifies the pre-discovery gateway,
    /// which still serves sessions and the terminal. Any other error is real.
    @discardableResult
    public func discover() async throws -> GatewayCapabilities {
        let discovered: GatewayCapabilities
        do {
            discovered = try await get(path: "/v1/capabilities")
        } catch LatchError.http(let status, _, _) where status == 404 {
            discovered = GatewayCompatibility.legacyCapabilities()
        }
        try GatewayCompatibility.validate(discovered)
        capabilities = discovered
        return discovered
    }

    /// Drops the cached discovery document.
    ///
    /// Required after a transport interruption or path migration: capabilities
    /// may have changed while the phone was offline, and a reconnect must not
    /// carry the old answers across.
    public func invalidateDiscovery() {
        capabilities = nil
    }

    private func requireDiscovery() async throws -> GatewayCapabilities {
        if let capabilities { return capabilities }
        return try await discover()
    }

    private func require(_ endpoint: GatewayEndpointsName) async throws {
        let capabilities = try await requireDiscovery()
        guard GatewayCompatibility.supports(endpoint: endpoint, capabilities: capabilities) else {
            throw LatchError.endpointUnavailable(endpoint)
        }
    }

    // MARK: - Sessions

    public func listSessions() async throws -> [SessionSummary] {
        try await require(.sessions)
        let report: ListReport = try await get(path: "/v1/sessions")
        return report.sessions
    }

    public func sessionCapabilities(sessionId: String) async throws -> SessionCapabilities {
        try await require(.sessionCapabilities)
        return try await get(path: "/v1/sessions/\(escape(sessionId))/capabilities")
    }

    // MARK: - Sending

    /// Submits one operation.
    ///
    /// `idempotencyKey` must be reused unchanged when retrying after an
    /// ambiguous failure; that is the whole point of the header. A fresh key is
    /// minted for a genuinely new submission. The gateway rejects the header on
    /// `keys`, which has no retry contract, so it is omitted there.
    public func send(
        sessionId: String,
        operation: SendOperation,
        idempotencyKey: String? = nil
    ) async throws -> SendReport {
        try await require(.send)
        var headers: [String: String] = [:]
        if operation.acceptsIdempotencyKey,
           GatewayCompatibility.supports(feature: .idempotencyKeys, capabilities: capabilities) {
            headers[LatchContract.idempotencyKeyHeader] = idempotencyKey ?? Self.newIdempotencyKey()
        }
        let body = try JSONEncoder().encode(operation.body)
        return try await request(
            path: "/v1/sessions/\(escape(sessionId))/send",
            method: "POST",
            body: body,
            headers: headers
        )
    }

    /// A fresh submission key that satisfies `idempotency-key.schema.json`.
    public static func newIdempotencyKey() -> String {
        // A UUID string is already within the schema's length and character
        // limits, so no further encoding is needed.
        UUID().uuidString
    }

    // MARK: - Events

    /// Opens the event stream for one session.
    ///
    /// The caller owns the returned stream and must `close()` it. Discovery is
    /// checked first so a gateway without `events` reports that plainly rather
    /// than failing a WebSocket handshake.
    public func events(
        sessionId: String,
        cursor: Int = 0,
        connectorEpoch: Int? = nil
    ) async throws -> EventStream {
        try await require(.events)
        return EventStream(
            link: link,
            sessionId: sessionId,
            cursor: cursor,
            connectorEpoch: connectorEpoch,
            session: session
        )
    }

    // MARK: - Transport

    private func escape(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? value
    }

    private func get<T: Decodable>(path: String) async throws -> T {
        try await request(path: path, method: "GET", body: nil, headers: [:])
    }

    private func request<T: Decodable>(
        path: String,
        method: String,
        body: Data?,
        headers: [String: String]
    ) async throws -> T {
        guard let url = URL(string: link.url.absoluteString + path) else {
            throw LatchError.invalidURL(link.url.absoluteString + path)
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        // The Noise proxy injects the supervised gateway credential on the
        // Mac. Supplying a credential to its loopback side is refused rather
        // than stripped, so an empty token deliberately means no header.
        if !link.token.isEmpty {
            request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw LatchError.transport(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw LatchError.malformedResponse("no HTTP status")
        }
        guard (200..<300).contains(http.statusCode) else {
            throw Self.error(status: http.statusCode, path: path, data: data)
        }
        do {
            return try decoder.decode(T.self, from: data)
        } catch {
            throw LatchError.malformedResponse(String(describing: error))
        }
    }

    /// Turns a gateway error body into the failure the UI should show.
    static func error(status: Int, path: String, data: Data) -> LatchError {
        let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        let code = body?["error"] as? String
        let reason = body?["reason"] as? String
            ?? code
            ?? String(data: data, encoding: .utf8)
            ?? ""
        if status == 401 || status == 403 {
            return .unauthorized
        }
        // 409 covers two different things. Only `error: "refused"` means the
        // harness declined the operation; an ambiguous session name is a
        // lookup failure and must not be shown as a refusal.
        if status == 409, code == "refused" {
            return .refused(reason.isEmpty ? "the harness refused this operation" : reason)
        }
        return .http(
            status: status,
            path: path,
            reason: reason.isEmpty ? "request failed" : reason
        )
    }
}
