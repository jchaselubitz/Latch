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
    private static let maximumJSONResponseBytes = 512 * 1024
    private let link: GatewayLink
    // Retaining the production transport keeps its capability listener and
    // app-scoped native link owner alive for this gateway's full lifetime.
    private let transport: (any GatewayTransport)?
    private let session: URLSession
    private let decoder = JSONDecoder()
    private var capabilities: GatewayCapabilities?

    init(link: GatewayLink, session: URLSession = .shared) {
        self.link = link
        self.transport = nil
        self.session = session
    }

    public init(transport: any GatewayTransport, session: URLSession = .shared) {
        link = transport.gatewayLink
        self.transport = transport
        self.session = session
    }

    public var gateway: GatewayLink { link }
    public var discovered: GatewayCapabilities? { capabilities }

    /// Stops the loopback adapter behind this gateway. Its capability is gone
    /// with it; a later request through this gateway fails locally.
    public func stopTransport() {
        transport?.stop()
    }

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

    /// Lists one bounded page of directories on the Mac. Query construction
    /// belongs here so canonical paths containing spaces, Unicode, `#`, or
    /// `?` never become hand-built URL syntax.
    public func browseDirectories(
        path: String? = nil,
        cursor: String? = nil
    ) async throws -> DirectoryPage {
        try await require(.browseDirectories)
        return try await request(
            method: "GET",
            path: "/v2/directories",
            queryItems: [
                path.map { URLQueryItem(name: "path", value: $0) },
                cursor.map { URLQueryItem(name: "cursor", value: $0) }
            ].compactMap { $0 }
        )
    }

    /// Creates the gateway's standard login shell. The request ID is supplied
    /// by the caller because it must survive a lost response and its retry.
    public func createSession(requestID: UUID, cwd: String) async throws -> CreateReport {
        try await require(.createSession)
        let body: Data
        do {
            body = try JSONEncoder().encode(CreateSessionRequest(requestId: requestID, cwd: cwd))
        } catch {
            throw LatchError.malformedResponse(String(describing: error))
        }
        return try await request(method: "POST", path: "/v2/sessions", body: body)
    }

    /// Stops one session on the Mac, leaving its record and dead pane behind.
    ///
    /// This ends what is running, which is why it sits at the control grant
    /// alongside the terminal: a device that may not type into the pane may
    /// not end what is in it either. The gateway sends SIGTERM and escalates
    /// on its own, so there is no signal, force flag, or removal to choose
    /// here — the phone asks for a stop and is told what happened.
    ///
    /// It is safe to repeat: a session that has already exited answers the
    /// same way, so a request whose response was lost can simply be sent
    /// again.
    public func stopSession(sessionID: String) async throws -> SessionStopReport {
        try await require(.stopSession)
        return try await request(method: "POST", path: "/v2/sessions/\(sessionID)/stop")
    }

    /// Reads the session's live pane once, without attaching.
    ///
    /// This is the only terminal-shaped call an observing device may make. It
    /// is a capture, not an attach: it steals nothing, which is what lets the
    /// phone show the user what is on the Mac *before* asking whether to take
    /// the surface away from it.
    ///
    /// `scrollbackLines` is a request, not a guarantee. The gateway caps it
    /// and ignores it entirely while a full-screen application owns the pane,
    /// which has no scrollback to read; the answer reports what was actually
    /// included.
    public func previewSession(
        sessionID: String,
        scrollbackLines: Int = 0
    ) async throws -> SessionPreview {
        try await require(.preview)
        let path = "/v2/sessions/\(sessionID)/preview"
        let queryItems = scrollbackLines > 0
            ? [URLQueryItem(name: "scrollbackLines", value: String(scrollbackLines))]
            : []
        return try await request(method: "GET", path: path, queryItems: queryItems)
    }

    /// Takes the session's terminal surface at the declared grid.
    ///
    /// This is a steal: the session has one exclusive surface, and opening
    /// this socket moves it here. That is why the size travels as a query
    /// parameter rather than a handshake frame — the gateway accepts both, and
    /// the query form skips a round trip during which an opened socket is
    /// holding a steal in reserve against its 10-second size deadline.
    ///
    /// The size is never guessed by this layer; the caller supplies the grid
    /// the pane already has.
    public func openTerminal(
        sessionID: String,
        cols: Int,
        rows: Int,
        resume: String? = nil
    ) async throws -> any TerminalSocketConnection {
        try await require(.terminal)
        guard var components = URLComponents(url: link.url, resolvingAgainstBaseURL: false) else {
            throw LatchError.invalidURL(link.url.absoluteString)
        }
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        components.path = "/v2/sessions/\(sessionID)/terminal"
        components.queryItems = [
            URLQueryItem(name: "cols", value: String(max(1, cols))),
            URLQueryItem(name: "rows", value: String(max(1, rows))),
            resume.map { URLQueryItem(name: "resume", value: $0) }
        ].compactMap { $0 }
        guard let url = components.url else {
            throw LatchError.invalidURL(link.url.absoluteString)
        }
        var request = URLRequest(url: url)
        if !link.token.isEmpty {
            request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        }
        return URLSessionTerminalSocketConnection(task: session.webSocketTask(with: request))
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
        try await request(method: "GET", path: path)
    }

    /// One bounded JSON request path for both manual HTTPS links and paired
    /// loopback links. Discovery, authorization, and error mapping therefore
    /// remain identical for reads and creation.
    private func request<T: Decodable>(
        method: String,
        path: String,
        queryItems: [URLQueryItem] = [],
        body: Data? = nil
    ) async throws -> T {
        guard var components = URLComponents(url: link.url, resolvingAgainstBaseURL: false) else {
            throw LatchError.invalidURL(link.url.absoluteString)
        }
        components.path = path
        components.queryItems = queryItems.isEmpty ? nil : queryItems
        guard let url = components.url else {
            throw LatchError.invalidURL(link.url.absoluteString + path)
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if !link.token.isEmpty {
            request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
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
        guard data.count <= Self.maximumJSONResponseBytes else {
            throw LatchError.malformedResponse("response exceeded the JSON size limit")
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
        // Another device owns this creation id. Retrying it will never work;
        // the phone must start a new intent under a new id.
        if status == 403, code == "request_id_foreign" {
            return .refused(reason.isEmpty ? "This request belongs to another device." : reason)
        }
        if status == 401 || (status == 403 && code != "unreadable_directory") {
            return .unauthorized
        }
        // The paired tunnel could not reach the Mac at all. That is a local
        // transport failure wearing an HTTP status, and the reason it carries
        // is already a sentence — a status line in front of it would only
        // bury the part the person can act on.
        if status == 502, code == RemoteLinkGatewayTransport.tunnelFailureCode {
            return .transport(reason.isEmpty ? "The connection to your Mac failed." : reason)
        }
        return .http(
            status: status,
            path: path,
            reason: reason.isEmpty ? "request failed" : reason
        )
    }
}
