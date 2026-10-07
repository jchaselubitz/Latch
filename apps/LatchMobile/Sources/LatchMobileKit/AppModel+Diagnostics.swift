import Foundation

// MARK: - Diagnostics

extension AppModel {
    /// Clears the path counters.
    ///
    /// Unlinking deliberately does not: the counters describe this phone's
    /// networks, not its relationship with one Mac, and a field run that
    /// re-pairs between scenarios should not silently lose its own evidence.
    public func resetRemotePathTally() {
        pathReporter.resetTally()
    }

    func record(_ sample: LinkStageSample) {
        coldOpen.observe(sample)
        recentStages.append(sample)
        if recentStages.count > 64 { recentStages.removeFirst(recentStages.count - 64) }
    }

    static func millis(since date: Date) -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince(date) * 1000))
    }

    /// Diagnostics-only: whether the LAN attempt is skipped so the relay path
    /// is the one measured. Applies from the next connection attempt.
    public func setDiagnosticsSkipLAN(_ skip: Bool) async {
        diagnosticsOptions.skipLAN = skip
        coldOpen.observe(skipLAN: skip)
        await coordinator.setOptions(diagnosticsOptions)
    }
}

extension AppModel: DiagnosticsSubject {
    public func diagnosticsRecoveryCycle(skipLAN: Bool) async throws -> (path: String?, stages: [LinkStageSample]) {
        await setDiagnosticsSkipLAN(skipLAN)
        let started = Date()
        suspendPairedTransport()
        let before = recentStages.count
        await resumeAfterSuspension()
        guard case .linked = linkState else {
            throw LatchError.transport("The link did not become usable after the cycle.")
        }
        var stages = Array(recentStages.dropFirst(before))
        stages.append(LinkStageSample(stage: .applicationReady, milliseconds: Self.millis(since: started)))
        return (linkSnapshot.path?.rawValue, stages)
    }

    public func diagnosticsSessionList() async throws -> LinkStageSample {
        guard let gateway else { throw LatchError.transport("Not linked to a computer.") }
        let (list, sample) = try await measureStage(.sessionList) { try await gateway.listSessions() }
        sessions = list
        return sample
    }

    public func diagnosticsPreview() async throws -> LinkStageSample? {
        guard let gateway, let session = sessions.first else { return nil }
        let (_, sample) = try await measureStage(.preview) {
            try await gateway.previewSession(sessionID: session.id, scrollbackLines: 0)
        }
        return sample
    }

    public func diagnosticsTerminalFirstOutput() async throws -> LinkStageSample? {
        guard surface.terminal, terminalUnlock.isUnlocked,
              let session = sessions.first(where: \.isRunning),
              let terminal = terminalSession(for: session)
        else { return nil }
        defer { discardTerminal(for: session) }
        let started = Date()
        terminal.attach(cols: 80, rows: 24)
        let deadline = Date().addingTimeInterval(10)
        for await _ in terminal.output {
            return LinkStageSample(stage: .terminalFirstOutput, milliseconds: Self.millis(since: started))
        }
        _ = deadline
        throw LatchError.transport("No terminal output arrived.")
    }
}
