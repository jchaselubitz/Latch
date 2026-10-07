import Foundation

// MARK: - Terminals

extension AppModel {
    /// Whether the terminal is open to this device right now: it holds the
    /// Mac's grant *and* has passed the owner check recently enough.
    public var isTerminalUnlocked: Bool { surface.terminal && terminalUnlock.isUnlocked }

    /// Why the last owner check did not open the terminal, when there is
    /// something worth saying. A cancelled prompt leaves this nil.
    public var terminalUnlockFailure: String? { terminalUnlock.failure }

    /// Why the shared owner check could not authorize a protected action.
    public var ownerAuthenticationFailure: String? { terminalUnlock.failure }

    /// Asks the device owner to confirm before a terminal is opened.
    ///
    /// Only taking the terminal asks this. Observing a conversation, sending
    /// to it, and answering its prompts never do: they go through the
    /// Conversation Hub and open no terminal.
    ///
    /// Called by the terminal screen ahead of `terminalSession(for:)`. Inside
    /// the grace window it answers without prompting, so attaching, reading
    /// something else, and reattaching is one Face ID check rather than three.
    @discardableResult
    public func unlockTerminal() async -> Bool {
        guard surface.terminal else { return false }
        return await unlockRemoteAccess(
            reason: "Take this session's terminal from your Mac and type into it."
        )
    }

    /// One owner-authentication grace window covers terminal access, revealing
    /// remote folder names, and creating a process. Capability and grant checks
    /// still run independently for every operation.
    @discardableResult
    public func unlockRemoteAccess(reason: String) async -> Bool {
        await terminalUnlock.unlock(reason: reason)
    }

    /// Returns the one terminal connection for this session, or nil when this
    /// device may not open one — gated on `surface.terminal`, the way
    /// `conversationStore(for:)` is gated on the conversation endpoint, and on
    /// a current owner check, which `unlockTerminal()` is what obtains.
    public func terminalSession(for session: SessionSummary) -> TerminalSession? {
        guard let gateway, surface.terminal, terminalUnlock.isUnlocked else { return nil }
        if let existing = terminalSessions[session.id] { return existing }
        let id = session.id
        let connector = terminalConnector
        let created = TerminalSession(sessionID: id) { [weak self] cols, rows, resume in
            if let connector {
                return try await connector(id, cols, rows)
            }
            // The gateway of the moment, not the one captured at creation: a
            // resume after transport loss goes through the replacement link.
            guard let current = await self?.gateway else {
                throw LatchError.transport("Not linked to a computer.")
            }
            return try await current.openTerminal(sessionID: id, cols: cols, rows: rows, resume: resume)
        }
        terminalSessions[id] = created
        return created
    }

    /// After a link comes back: terminals whose attach was interrupted and
    /// whose bounded resume capability is still valid are resumed; the
    /// gateway refuses if anyone else has attached since. Every other
    /// interrupted terminal stays put with a Reconnect button. Nothing typed
    /// is replayed.
    @discardableResult
    public func resumeInterruptedTerminals() -> Int {
        var resumed = 0
        for terminal in terminalSessions.values where terminal.canResume {
            if terminal.resume() { resumed += 1 }
        }
        return resumed
    }

    /// Transport loss under held terminals: they become interrupted rather
    /// than silently closed, and the person is told input may be missing.
    func interruptTerminals() {
        for terminal in terminalSessions.values where terminal.holdsSurface {
            terminal.interrupt()
        }
    }

    /// Reads the pane without attaching, so nothing is taken from the Mac.
    ///
    /// This is the first thing the terminal screen does, and it is deliberately
    /// not gated on `surface.terminal`: the route needs only `observe`, so a
    /// phone that may never attach can still see what it cannot type at.
    public func previewSession(
        for session: SessionSummary,
        scrollbackLines: Int = 0
    ) async throws -> SessionPreview {
        guard let gateway else { throw LatchError.transport("Not linked to a computer.") }
        return try await gateway.previewSession(
            sessionID: session.id,
            scrollbackLines: scrollbackLines
        )
    }

    /// How long a held terminal survives with no input once the app stops
    /// being the thing on screen.
    ///
    /// Backgrounding proper releases the surface at once — a phone in a pocket
    /// is not using a terminal. This covers the other case: an app that is on
    /// screen but not frontmost, which is what a pulled-down notification
    /// centre, an incoming call banner, the app switcher, and the Face ID
    /// prompt itself all produce. Tearing the terminal down for those would
    /// make the phone unusable; holding it forever would leave the Mac's one
    /// surface parked on a phone nobody is looking at.
    public nonisolated static let terminalIdleTimeout: TimeInterval = 2 * 60

    /// How often the countdown checks. It bounds how late a release can be, so
    /// the surface comes back within a quarter-minute of the deadline rather
    /// than only when the app is next touched.
    private nonisolated static let terminalIdleTick: Duration = .seconds(15)

    /// Starts releasing idle terminals while the app is not frontmost.
    ///
    /// Idempotent: a scene phase that flickers does not restart the clock,
    /// because the clock is `lastInputAt` on each session rather than a
    /// countdown this task owns.
    public func beginTerminalIdleCountdown(
        timeout: TimeInterval = AppModel.terminalIdleTimeout
    ) {
        guard terminalIdleWatch == nil else { return }
        terminalIdleWatch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: AppModel.terminalIdleTick)
                guard !Task.isCancelled, let self else { return }
                self.releaseIdleTerminals(timeout: timeout)
            }
        }
    }

    /// Stops the countdown. Called when the app comes back to the front, and
    /// when backgrounding releases every surface outright.
    public func cancelTerminalIdleCountdown() {
        terminalIdleWatch?.cancel()
        terminalIdleWatch = nil
    }

    /// Releases every held surface that has had no input for `timeout`.
    ///
    /// The owner check goes with it. A terminal that was given up because
    /// nobody was typing at it should be reopened deliberately, and the grace
    /// window outlasts this timeout otherwise.
    @discardableResult
    public func releaseIdleTerminals(
        timeout: TimeInterval = AppModel.terminalIdleTimeout,
        now: Date = Date()
    ) -> Int {
        var released = 0
        for session in terminalSessions.values where session.holdsSurface {
            guard now.timeIntervalSince(session.lastInputAt) >= timeout else { continue }
            session.detach()
            released += 1
        }
        if released > 0 { terminalUnlock.lock() }
        return released
    }

    /// Releases every held surface before the app is suspended.
    ///
    /// The sessions themselves are kept, so foregrounding returns to
    /// `.closed(.detached)` with a Reattach button rather than silently taking
    /// the surface back from whoever is now using it.
    public func suspendTerminals() {
        cancelTerminalIdleCountdown()
        terminalSessions.values.forEach { $0.detach() }
    }

    /// Releases one session's surface and forgets the connection.
    ///
    /// This is what back-navigation uses rather than `detach()` alone: nothing
    /// displays the connection's state once the screen is gone, and a screen
    /// re-entered later re-reads the pane through the preview anyway. Keeping
    /// it would also leave a second reader on an output stream that only ever
    /// has one.
    public func discardTerminal(for session: SessionSummary) {
        guard let existing = terminalSessions.removeValue(forKey: session.id) else { return }
        existing.detach()
    }

    /// Releases every held surface and forgets the connections.
    ///
    /// This is the teardown path — unlinking, or a link that failed — so the
    /// grace window ends with it. A phone relinked to another Mac starts from
    /// a fresh owner check rather than inheriting one.
    public func detachAllTerminals() {
        cancelTerminalIdleCountdown()
        terminalSessions.values.forEach { $0.detach() }
        terminalSessions = [:]
        terminalUnlock.lock()
    }
}
