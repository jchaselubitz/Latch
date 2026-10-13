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
        let discovered = try await discoveredCapabilities()
        guard GatewayCompatibility.supports(endpoint: endpoint, capabilities: discovered) else {
            throw LatchError.endpointUnavailable(endpoint)
        }
    }

    private func discoveredCapabilities() async throws -> GatewayCapabilities {
        if let capabilities { return capabilities }
        return try await discover()
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
    /// Starts a session on the Mac: a standard shell, or `agent` launched
    /// directly in `cwd`. The Mac resolves the agent's executable; the phone
    /// names only the kind, and only one discovery listed under
    /// `features.sessionAgents`, so an older gateway is never asked for a
    /// field its contract closes against.
    ///
    /// `model` is one id from ``agentModels(for:)`` and travels only with an
    /// agent, and only to a Mac that serves model choice; `nil` starts the
    /// agent on whatever the Mac's owner configured.
    public func createSession(
        requestID: UUID,
        cwd: String,
        agent: SessionAgent? = nil,
        model: String? = nil
    ) async throws -> CreateReport {
        try await require(.createSession)
        if let agent {
            let discovered = try await discoveredCapabilities()
            guard discovered.features.sessionAgents.contains(agent) else {
                throw LatchError.agentUnavailable(agent)
            }
        }
        let model = agent == nil ? nil : model
        if model != nil {
            try await require(.agentModels)
        }
        let body: Data
        do {
            body = try JSONEncoder().encode(
                CreateSessionRequest(requestId: requestID, cwd: cwd, agent: agent, model: model)
            )
        } catch {
            throw LatchError.malformedResponse(String(describing: error))
        }
        return try await request(method: "POST", path: "/v2/sessions", body: body)
    }

    /// The models `agent` can start with on the Mac, read from the agent's
    /// own model cache when asked, so the list is as current as the CLI that
    /// will run it rather than as old as this app's build.
    public func agentModels(for agent: SessionAgent) async throws -> AgentModelCatalog {
        try await require(.agentModels)
        return try await get(path: "/v2/agents/\(agent.rawValue)/models")
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

    // MARK: Git (read-only)

    /// Repositories the Mac offers to open: the working trees of its running
    /// sessions, and the ones any paired phone opened recently. The Mac keeps
    /// that list; this phone stores none of it.
    public func gitRepositories() async throws -> GitRepositoryList {
        try await require(.gitRepositories)
        return try await get(path: "/v2/git/repositories")
    }

    /// Resolves a folder on the Mac to the root of its Git working tree, or
    /// says why it is not one (a plain folder, a bare repository).
    public func resolveGitRepository(path: String) async throws -> GitRepository {
        try await require(.gitResolve)
        return try await request(
            method: "GET",
            path: "/v2/git/resolve",
            queryItems: [URLQueryItem(name: "path", value: path)]
        )
    }

    /// One working tree's status. `path` may be any folder inside it; the
    /// Mac resolves it again on every call. Reading never fetches.
    public func gitStatus(path: String) async throws -> GitStatus {
        try await require(.gitStatus)
        return try await request(
            method: "GET",
            path: "/v2/git/status",
            queryItems: [URLQueryItem(name: "path", value: path)]
        )
    }

    /// One page of one changed file's diff. The file is named by the exact
    /// bytes the status listed (`pathKey`), never by its display name.
    public func gitDiff(
        path: String,
        fileKey: String,
        area: GitChangeArea,
        cursor: String? = nil
    ) async throws -> GitFileDiff {
        try await require(.gitDiff)
        return try await request(
            method: "GET",
            path: "/v2/git/diff",
            queryItems: [
                URLQueryItem(name: "path", value: path),
                URLQueryItem(name: "file", value: fileKey),
                URLQueryItem(name: "area", value: area.rawValue),
                cursor.map { URLQueryItem(name: "cursor", value: $0) }
            ].compactMap { $0 }
        )
    }

    // MARK: Git (changes)

    /// Largest mutation body the phone sends. The paired proxy buffers whole
    /// requests under 32 KiB; this leaves room for its headers.
    static let maximumGitMutationBodyBytes = 30 * 1024

    /// Local branches of the working tree at `path`. Never fetches.
    public func gitBranches(path: String) async throws -> GitBranchList {
        try await require(.gitBranches)
        return try await request(
            method: "GET",
            path: "/v2/git/branches",
            queryItems: [URLQueryItem(name: "path", value: path)]
        )
    }

    /// The Sync sheet's preview: the upstream as last fetched, ahead/behind,
    /// and a page of incoming commits. Never fetches. A next page whose
    /// upstream moved throws `.git("stale_cursor")`; start again without one.
    public func gitSync(path: String, cursor: String? = nil) async throws -> GitSyncPreview {
        try await require(.gitSync)
        return try await request(
            method: "GET",
            path: "/v2/git/sync",
            queryItems: [
                URLQueryItem(name: "path", value: path),
                cursor.map { URLQueryItem(name: "cursor", value: $0) }
            ].compactMap { $0 }
        )
    }

    /// Sends one Git change. The answer is always the Mac's receipt for it,
    /// pending when the Mac is still running it (a repeat of a request it is
    /// already working on, or a fetch, merge or rebase that outlasted the
    /// Mac's wait: `202` with Git's progress); a refusal before acceptance
    /// throws `.git`.
    public func gitMutation(_ action: GitMutationAction) async throws -> GitMutationReceipt {
        try await require(.gitReceipts)
        let encoder = JSONEncoder()
        let body: Data
        let route: String
        do {
            switch action {
            case .stage(let request):
                try await require(.gitStage)
                // Marking a conflicted file resolved is its own capability:
                // an older Mac's contract refuses the area and the flag.
                if action.stagesConflictedFiles {
                    try await require(.gitStageResolved)
                }
                route = "/v2/git/stage"
                body = try encoder.encode(request)
            case .unstage(let request):
                try await require(.gitUnstage)
                route = "/v2/git/unstage"
                body = try encoder.encode(request)
            case .commit(let request):
                try await require(.gitCommit)
                route = "/v2/git/commit"
                body = try encoder.encode(request)
            case .createBranch(let request):
                try await require(.gitCreateBranch)
                route = "/v2/git/branches/create"
                body = try encoder.encode(request)
            case .switchBranch(let request):
                try await require(.gitSwitchBranch)
                route = "/v2/git/branches/switch"
                body = try encoder.encode(request)
            case .fetch(let request):
                try await require(.gitFetch)
                route = "/v2/git/fetch"
                body = try encoder.encode(request)
            case .fastForward(let request):
                try await require(.gitFastForward)
                route = "/v2/git/fast-forward"
                body = try encoder.encode(request)
            case .merge(let request):
                try await require(.gitMerge)
                route = "/v2/git/merge"
                body = try encoder.encode(request)
            case .rebase(let request):
                try await require(.gitRebase)
                route = "/v2/git/rebase"
                body = try encoder.encode(request)
            case .continueOperation(let request):
                try await require(.gitContinue)
                route = "/v2/git/continue"
                body = try encoder.encode(request)
            case .abortOperation(let request):
                try await require(.gitAbort)
                route = "/v2/git/abort"
                body = try encoder.encode(request)
            case .push(let request):
                try await require(.gitPush)
                route = "/v2/git/push"
                body = try encoder.encode(request)
            }
        } catch let error as LatchError {
            throw error
        } catch {
            throw LatchError.malformedResponse(String(describing: error))
        }
        guard body.count <= Self.maximumGitMutationBodyBytes else {
            throw LatchError.git(
                code: "request_too_large",
                reason: "That is too many files to change at once from the phone. Use Stage all, or fewer files."
            )
        }
        return try await request(method: "POST", path: route, body: body)
    }

    /// The push review: what a push of the current branch would send, to
    /// its upstream or, when `remote` and `destination` are given, to the
    /// destination the person chose. Never contacts the remote. A next page
    /// whose refs moved throws `.git("stale_cursor")`.
    public func gitPushPreview(
        path: String, remote: String? = nil, destination: String? = nil, cursor: String? = nil
    ) async throws -> GitPushPreview {
        try await require(.gitPushPreview)
        var items = [URLQueryItem(name: "path", value: path)]
        // The Mac takes both or neither.
        if let remote, let destination {
            items.append(URLQueryItem(name: "remote", value: remote))
            items.append(URLQueryItem(name: "destination", value: destination))
        }
        if let cursor { items.append(URLQueryItem(name: "cursor", value: cursor)) }
        return try await request(method: "GET", path: "/v2/git/push/preview", queryItems: items)
    }

    /// Sends a reviewed push. Same receipts as every other change.
    public func gitPush(_ request: GitPushRequest) async throws -> GitMutationReceipt {
        try await gitMutation(.push(request))
    }

    /// Asks the Mac to check the remote for a push it could not confirm.
    /// `.git("still_running")` while Git is still running that push.
    public func gitPushReconcile(requestId: String) async throws -> GitPushReconciliation {
        try await require(.gitReceipts)
        try await require(.gitPushReconcile)
        let body: Data
        do {
            body = try JSONEncoder().encode(GitPushReconcileRequest(requestId: requestId))
        } catch {
            throw LatchError.malformedResponse(String(describing: error))
        }
        return try await request(method: "POST", path: "/v2/git/push/reconcile", body: body)
    }

    /// This device's receipt for one Git change. `.git("receipt_not_found")`
    /// means the Mac never accepted that request id.
    public func gitReceipt(requestID: String) async throws -> GitMutationReceipt {
        try await require(.gitReceipts)
        guard let encoded = requestID.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(["-"])) else {
            throw LatchError.git(code: "invalid_request", reason: "That request id is malformed.")
        }
        return try await get(path: "/v2/git/receipts/\(encoded)")
    }

    /// Tells the Mac which key to seal this device's attention alerts to.
    ///
    /// The device is the one the link authenticated, never a field here, so
    /// a phone can only ever set its own key. Repeating it is harmless.
    public func registerNotificationKey(publicKeyHex: String) async throws {
        try await require(.notificationKey)
        let body = try JSONEncoder().encode(["publicKey": publicKeyHex])
        let _: NoContent = try await request(method: "PUT", path: "/v2/notifications/key", body: body)
    }

    /// Places one file in the session's working directory on the Mac, under
    /// its attachments folder, so the next message can name it by path.
    ///
    /// The Mac chooses the final name — `name` is a suggestion it reduces to
    /// a safe alphabet and makes unique — and answers with where the file
    /// landed. Nothing is sent to the agent here; the caller does that next.
    public func uploadAttachment(
        sessionID: String,
        name: String,
        data: Data
    ) async throws -> AttachmentReceipt {
        try await require(.attachments)
        return try await request(
            method: "POST",
            path: "/v2/sessions/\(sessionID)/attachments",
            queryItems: [URLQueryItem(name: "name", value: ConversationAttachment.suggestedName(name))],
            body: data,
            contentType: "application/octet-stream"
        )
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
        body: Data? = nil,
        contentType: String = "application/json"
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
            request.setValue(contentType, forHTTPHeaderField: "Content-Type")
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
        if data.isEmpty, let empty = NoContent() as? T {
            return empty
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
        // The Mac serves agents but could not find this one. The reason is
        // already the sentence to show, and nothing changes until someone
        // installs it on the Mac.
        if code == "agent_unavailable" {
            return .refused(
                reason.isEmpty ? "That agent is not installed on your Mac." : reason
            )
        }
        // The model list the phone chose from has moved on. The sentence says
        // to choose again; the picker re-reads the list on its own.
        if code == "model_unavailable" {
            return .refused(
                reason.isEmpty ? "Your Mac no longer offers that model. Choose another." : reason
            )
        }
        // An attachment the Mac would not place: too large, or the session's
        // folder is gone. The reason is the sentence to show, and sending the
        // same file again will not change it.
        if let code, ["attachment_too_large", "workspace_unavailable", "attachments_folder_unsafe"]
            .contains(code) {
            return .refused(reason.isEmpty ? "Your Mac could not save that attachment." : reason)
        }
        // A Git route's refusal is a sentence written for the phone; the
        // code says which one, so a screen can act on it (a stale cursor
        // reloads the diff) without parsing the words.
        if path.hasPrefix("/v2/git/"), let code, status != 401, status != 403 || code == "unreadable_directory" {
            return .git(code: code, reason: reason.isEmpty ? "Git on your Mac could not answer." : reason)
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

/// The answer of a route that answers with no body (`204 No Content`).
struct NoContent: Decodable {
    init() {}
    init(from decoder: Decoder) throws {}
}
