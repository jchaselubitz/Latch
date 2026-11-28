import Foundation

// MARK: - Sessions

extension AppModel {
    /// What discovery permits on the session screen.
    public var surface: SessionSurface {
        guard case .linked(let capabilities) = linkState else {
            return SessionSurface(chat: false, composer: false, interactionControls: false)
        }
        return GatewayCompatibility.sessionSurface(for: capabilities)
            .restricted(to: pairedDevice?.permission)
    }

    /// What the Mac offered when it was last heard from. While the link is
    /// recovering this keeps an open conversation or terminal in place, so
    /// it shows its own reconnecting state instead of being swapped for an
    /// "update needed" screen the moment the link drops. Only routing reads
    /// it; controls that act on the Mac still follow `surface`.
    var routingSurface: SessionSurface {
        guard let capabilities = linkState.cachedCapabilities else { return surface }
        return GatewayCompatibility.sessionSurface(for: capabilities)
            .restricted(to: pairedDevice?.permission)
    }

    /// Where a tap on this session row goes.
    public func route(for session: SessionSummary) -> SessionRoute {
        SessionRoute.route(
            preference: sessionPresentation,
            connector: session.connector,
            surface: routingSurface,
            isRunning: session.isRunning,
            linkReady: linkState.isUsable
        )
    }

    /// A link named a session. The list is re-read when the route is usable —
    /// the session is often seconds old — and otherwise the request waits for
    /// the listing that follows discovery. Either way the view opens it through
    /// `takeRequestedSession()`, the same route a tap on the row takes.
    public func requestSession(id sessionID: String) async {
        requestedSessionID = sessionID
        requestedSessionError = nil
        guard linkState.isUsable else { return }
        await refreshSessions()
    }

    /// The requested session once the list holds it, clearing the request.
    public func takeRequestedSession() -> SessionSummary? {
        guard let sessionID = requestedSessionID,
              let session = sessions.first(where: { $0.id == sessionID }) else { return nil }
        requestedSessionID = nil
        return session
    }

    public func clearRequestedSessionError() {
        requestedSessionError = nil
    }

    /// Whether the linked Mac serves the stop route at all, independent of
    /// this phone's grant. A Mac that predates the route advertises nothing,
    /// and there is then no control to show and nothing to explain.
    public var advertisesSessionStop: Bool { linkedSupports(.stopSession) }

    /// Whether a live link's Mac advertises `endpoint`. False while the link
    /// is anything but linked, whatever was advertised before.
    func linkedSupports(_ endpoint: GatewayEndpointsName) -> Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: endpoint, capabilities: capabilities)
    }

    /// What every control-gated surface says when the grant, not the Mac, is
    /// what holds it back.
    static let controlGrantExplanation = """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """

    /// Whether this phone may stop a session right now. Ending what is running
    /// is a control operation, held to the same grant as the terminal.
    public var canStopSessions: Bool {
        advertisesSessionStop && pairedDevice?.permission.permits(.control) == true
    }

    /// Why the advertised stop control cannot be used, or nil when it can.
    /// Only a grant can hold it back once the route exists.
    public var sessionStopUnavailableExplanation: String? {
        guard advertisesSessionStop, !canStopSessions else { return nil }
        return Self.controlGrantExplanation
    }

    /// Asks the Mac to stop one session, then re-reads the list.
    ///
    /// Stopping is not removing: the Mac keeps the session's record and its
    /// dead pane, so the row stays and turns `exited` rather than vanishing.
    /// Any terminal this phone holds for the session is dropped first — the
    /// surface is about to end underneath it, and a connection kept past that
    /// only produces a socket close nobody is watching.
    ///
    /// The request is safe to repeat, so a lost response costs a retry and
    /// nothing else. Returns whether the Mac confirmed the stop.
    @discardableResult
    public func stopSession(_ session: SessionSummary) async -> Bool {
        guard let gateway, canStopSessions else {
            // Reached when the grant or the link changed between the row
            // offering Stop and the confirmation coming back, so the reason
            // has to name which of the two it was.
            sessionsError = sessionStopUnavailableExplanation
                ?? (linkState.isUsable
                    ? "This Mac cannot stop sessions from a phone. Update Latch on the Mac."
                    : "This phone is not connected to your Mac right now.")
            return false
        }
        // A second tap on a row already waiting is not a second stop.
        guard stoppingSessionIDs.insert(session.id).inserted else { return false }
        defer { stoppingSessionIDs.remove(session.id) }
        discardTerminal(for: session)
        do {
            _ = try await gateway.stopSession(sessionID: session.id)
            sessionsError = nil
            await refreshSessions()
            return true
        } catch {
            let failure = (error as? LatchError)?.message ?? error.localizedDescription
            // The stop may still have landed before the answer was lost, so
            // the list is re-read either way rather than left showing a stale
            // row. The reason the stop failed is restored afterwards: a clean
            // refresh must not quietly clear the only account of it.
            await refreshSessions()
            sessionsError = failure
            return false
        }
    }

    private var savedChatPairingID: String? {
        guard loadsSavedChatCatalog, linkedSupports(.savedChats), linkedSupports(.savedChatResume),
              let pairedDevice, pairedDevice.isActive, pairedDevice.permission.permits(.control) else { return nil }
        return pairedDevice.deviceId
    }

    private var visibleSavedChats: [SavedChatSummary] {
        guard let catalog = savedChatCatalog, catalog.gateway === gateway,
              catalog.pairingID == savedChatPairingID else { return [] }
        return catalog.chats
    }

    public func isSavedChatVisible(_ chat: SavedChatSummary) -> Bool {
        visibleSavedChats.contains { $0.chatId == chat.chatId && $0.sessionId == chat.sessionId }
    }

    public var sessionBrowserSessions: [SessionSummary] {
        let retained = Set(sessions.map(\.id))
        return sessions + visibleSavedChats.filter { !retained.contains($0.sessionId) }.map { chat in
            SessionSummary(
                id: chat.sessionId, name: chat.name, title: chat.title, state: "exited",
                cwd: chat.cwd, commandLabel: chat.agent.rawValue, createdAt: chat.createdAt,
                connector: .named(chat.agent.rawValue)
            )
        }
    }

    public func savedChat(for session: SessionSummary) -> SavedChatSummary? {
        guard !session.isRunning else { return nil }
        return visibleSavedChats.first { $0.sessionId == session.id }
    }

    /// Confirms the new terminal's native identity before navigating to it.
    public func resumeSavedChat(_ chat: SavedChatSummary) async throws -> SessionSummary {
        let unavailable = LatchError.refused("This saved chat is no longer available on this connection.")
        guard let gateway, let pairingID = savedChatPairingID, chat.canResume,
              visibleSavedChats.contains(chat) else { throw unavailable }
        guard resumingChatIDs.insert(chat.chatId).inserted else {
            throw LatchError.refused("This chat is already resuming.")
        }
        defer { resumingChatIDs.remove(chat.chatId) }

        let operation = UUID()
        let report: CreateReport
        do {
            report = try await gateway.resumeSavedChat(chatID: chat.chatId, requestID: operation)
        } catch LatchError.transport {
            guard self.gateway === gateway, pairingID == savedChatPairingID else { throw unavailable }
            // A lost response may already have launched the terminal.
            report = try await gateway.resumeSavedChat(chatID: chat.chatId, requestID: operation)
        }

        let clock = ContinuousClock()
        let deadline = clock.now + .seconds(20)
        repeat {
            guard self.gateway === gateway, pairingID == savedChatPairingID else { throw unavailable }
            let status = try await gateway.savedChatStatus(chatID: chat.chatId)
            if status.sessionId == report.session.id, status.verified, !status.canResume,
               status.reason == nil, status.target?.agent == chat.agent, status.target?.cwd == chat.cwd {
                await refreshSessions()
                guard self.gateway === gateway, pairingID == savedChatPairingID else { throw unavailable }
                if let session = sessions.first(where: { $0.id == report.session.id && $0.isRunning }) {
                    return session
                }
            }
            try await Task.sleep(for: .milliseconds(400))
        } while clock.now < deadline
        throw LatchError.refused("The Mac did not confirm the resumed chat. Refresh Sessions to check its terminal.")
    }

    private func refreshSavedChats(from gateway: LatchGateway) async {
        guard let pairingID = savedChatPairingID else {
            savedChatCatalog = nil
            return
        }
        let result = try? await gateway.savedChats()
        guard self.gateway === gateway, pairingID == savedChatPairingID else { return }
        savedChatCatalog = (gateway, pairingID, result?.chats ?? [])
    }

    /// Refreshes session state for views that show errors through `sessionsError`.
    public func refreshSessions() async {
        guard gateway != nil, linkState.isUsable else { return }
        _ = try? await reloadSessions()
    }

    /// Returns a fresh session list and updates the same list used by the
    /// model's conversation stores. A request against an invalidated link
    /// cannot supply a result to the caller.
    @discardableResult
    public func reloadSessions() async throws -> [SessionSummary] {
        let unavailable = LatchError.transport("This phone is not connected to your Mac right now.")
        guard let gateway, linkState.isUsable else { throw unavailable }
        let isCurrent = { self.gateway === gateway && self.linkState.isUsable }
        do {
            let started = Date()
            let next = try await gateway.listSessions()
            guard isCurrent() else { throw unavailable }
            sessions = next
            await refreshSavedChats(from: gateway)
            guard isCurrent() else { throw unavailable }
            record(LinkStageSample(stage: .sessionList, milliseconds: Self.millis(since: started)))
            sessionsStale = false
            sessionsError = nil
            // A fresh list that still lacks a linked session means the Mac
            // does not have it: it ended and was removed, or it belongs to a
            // different computer than the one this phone is paired with.
            if let requested = requestedSessionID, !sessions.contains(where: { $0.id == requested }) {
                requestedSessionID = nil
                requestedSessionError = "Session \(requested) isn't on \(pairedDevice?.mac.displayName ?? "your Mac"). It may have been removed, or it may be running on a different computer."
            }
            return next
        } catch {
            guard isCurrent() else { throw unavailable }
            sessionsError = (error as? LatchError)?.message ?? error.localizedDescription
            throw error
        }
    }
}
