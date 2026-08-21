import Foundation

public struct GatewayLink: Equatable, Sendable, Codable {
    public let url: URL
    public let token: String

    public init(url: URL, token: String) {
        self.url = url
        self.token = token
    }

    public init(address: String, token: String) throws {
        let trimmed = address.trimmingCharacters(in: .whitespacesAndNewlines)
        var text = trimmed
        while text.hasSuffix("/") { text.removeLast() }
        guard let url = URL(string: text),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              url.host != nil
        else { throw LatchError.invalidURL(trimmed) }
        self.url = url
        self.token = token.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Protocol-major-2 gateway discovery and session client.
public actor LatchGateway {
    private let link: GatewayLink
    private let transport: any GatewayTransport
    private let session: URLSession
    private let decoder = JSONDecoder()
    private var capabilities: GatewayCapabilities?

    public init(link: GatewayLink, session: URLSession = .shared) {
        transport = HTTPSGatewayTransport(link: link)
        self.link = link
        self.session = session
    }

    public init(transport: any GatewayTransport, session: URLSession = .shared) {
        self.transport = transport
        link = transport.gatewayLink
        self.session = session
    }

    public var gateway: GatewayLink { link }
    public var discovered: GatewayCapabilities? { capabilities }

    @discardableResult
    public func discover() async throws -> GatewayCapabilities {
        let discovered: GatewayCapabilities = try await get(path: "/v2/capabilities")
        try GatewayCompatibility.validate(discovered)
        capabilities = discovered
        return discovered
    }

    public func invalidateDiscovery() { capabilities = nil }

    private func require(_ endpoint: GatewayEndpointsName) async throws {
        let discovered: GatewayCapabilities
        if let capabilities {
            discovered = capabilities
        } else {
            discovered = try await discover()
        }
        guard GatewayCompatibility.supports(endpoint: endpoint, capabilities: discovered) else {
            throw LatchError.endpointUnavailable(endpoint)
        }
    }

    public func listSessions() async throws -> [SessionSummary] {
        try await require(.sessions)
        let report: ListReport = try await get(path: "/v2/sessions")
        return report.sessions
    }

    /// Opens the sole v2 conversation channel. The stored resume tuple goes on
    /// the upgrade URL because the Hub must be able to speak first.
    public func openConversation(
        sessionID: String,
        position: ConversationResumePosition
    ) async throws -> any ConversationSocketConnection {
        try await require(.conversation)
        guard var components = URLComponents(url: link.url, resolvingAgainstBaseURL: false) else {
            throw LatchError.invalidURL(link.url.absoluteString)
        }
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        components.path = "/v2/sessions/\(sessionID)/conversation"
        components.queryItems = [
            position.generation.map { URLQueryItem(name: "generation", value: $0) },
            position.afterRevision.map { URLQueryItem(name: "afterRevision", value: String($0)) },
            position.operationEpoch.map { URLQueryItem(name: "operationEpoch", value: $0) }
        ].compactMap { $0 }
        guard let url = components.url else {
            throw LatchError.invalidURL(link.url.absoluteString)
        }
        var request = URLRequest(url: url)
        if !link.token.isEmpty {
            request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        }
        return URLSessionConversationSocketConnection(task: session.webSocketTask(with: request))
    }

    private func get<T: Decodable>(path: String) async throws -> T {
        guard let url = URL(string: link.url.absoluteString + path) else {
            throw LatchError.invalidURL(link.url.absoluteString + path)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        if !link.token.isEmpty {
            request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
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

    static func error(status: Int, path: String, data: Data) -> LatchError {
        let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        let code = body?["error"] as? String
        let reason = body?["reason"] as? String
            ?? code
            ?? String(data: data, encoding: .utf8)
            ?? ""
        if GatewayCompatibility.isControlPlaneUnmatchedRoute(
            status: status,
            code: code,
            reason: reason
        ) { return .notAGateway }
        if status == 401 || status == 403 { return .unauthorized }
        return .http(
            status: status,
            path: path,
            reason: reason.isEmpty ? "request failed" : reason
        )
    }
}
