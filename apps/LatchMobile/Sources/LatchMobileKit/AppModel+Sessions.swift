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

    /// Where a tap on this session row goes.
    public func route(for session: SessionSummary) -> SessionRoute {
        SessionRoute.route(
            preference: sessionPresentation,
            connector: session.connector,
            surface: surface,
            isRunning: session.isRunning
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
    public var advertisesSessionStop: Bool {
        guard case .linked(let capabilities) = linkState else { return false }
        return GatewayCompatibility.supports(endpoint: .stopSession, capabilities: capabilities)
    }

    /// Whether this phone may stop a session right now. Ending what is running
    /// is a control operation, held to the same grant as the terminal.
    public var canStopSessions: Bool {
        advertisesSessionStop && pairedDevice?.permission.permits(.control) == true
    }

    /// Why the advertised stop control cannot be used, or nil when it can.
    /// Only a grant can hold it back once the route exists.
    public var sessionStopUnavailableExplanation: String? {
        guard advertisesSessionStop, !canStopSessions else { return nil }
        return """
        This phone does not currently have control of this Mac. Open Latch on your Mac, find \
        this phone under Remote Access, and set it to Control.
        """
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

    /// Reloads the session list.
    public func refreshSessions() async {
        guard let gateway, linkState.isUsable else { return }
        isLoadingSessions = true
        defer { isLoadingSessions = false }
        do {
            let started = Date()
            sessions = try await gateway.listSessions()
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
        } catch let error as LatchError {
            sessionsError = error.message
        } catch {
            sessionsError = error.localizedDescription
        }
    }
}
