import Foundation

// MARK: - Discovery

extension AppModel {
    /// Whether this phone may keep retrying on its own. False in terminal
    /// states so the UI can say what to do instead of showing a spinner.
    public var canRetryAutomatically: Bool {
        switch linkState {
        case .revoked, .pairingRequired, .incompatible: return false
        default: return true
        }
    }

    /// The gateway's product version, for Settings.
    public var productVersion: String? {
        guard case .linked(let capabilities) = linkState else { return nil }
        return capabilities.productVersion.isEmpty ? nil : capabilities.productVersion
    }

    /// Classifies a discovery failure. A protocol disagreement is the one
    /// failure where the computer is fine, so it gets its own state rather
    /// than a string the UI cannot tell apart from a dead network.
    static func linkFailure(_ error: LatchError) -> LinkState {
        if let mismatch = error.protocolMismatch { return .incompatible(mismatch) }
        return .failed(error.message)
    }

    func observeCoordinator() {
        coordinatorObservation?.cancel()
        let coordinator = self.coordinator
        coordinatorObservation = Task { [weak self] in
            for await snapshot in await coordinator.snapshots() {
                guard !Task.isCancelled, let self else { return }
                await self.apply(snapshot)
            }
        }
    }

    /// One place turns owner snapshots into screen state.
    private func apply(_ snapshot: RemoteLinkSnapshot) async {
        LinkTrace.shared.mark("app.apply.\(RemoteLinkCoordinator.traceWord(snapshot.state))")
        linkSnapshot = snapshot
        if snapshot.state == .ready,
           let permission = snapshot.permission,
           var updated = pairedDevice,
           updated.permission != permission {
            updated.permission = permission
            _ = applyPairedDeviceRecord(updated)
        }
        coldOpen.observe(path: snapshot.path)
        coldOpen.observe(snapshot.state)
        switch snapshot.state {
        case .ready:
            if let timings = snapshot.timings {
                record(LinkStageSample(stage: .admission, milliseconds: timings.admissionMs))
                record(LinkStageSample(stage: .connect, milliseconds: timings.connectMs))
                record(LinkStageSample(stage: .peerWait, milliseconds: timings.peerWaitMs))
                record(LinkStageSample(stage: .authenticate, milliseconds: timings.authenticateMs))
                record(LinkStageSample(stage: .linkReady, milliseconds: timings.linkReadyMs))
            }
            if let path = snapshot.path { pathReporter.report(path) }
            if snapshot.generation != discoveredGeneration {
                discoveredGeneration = snapshot.generation
                // Discovery is network work. It must not run inline here: the
                // owner's snapshots are applied one after another, and a
                // discovery (or session refresh) that outlives its link, for
                // example one cut off by a suspend, would hold every later
                // state transition hostage until the request timed out. The
                // generation guards inside discard a stale result.
                Task { [weak self] in await self?.discoverOnFreshLink() }
            }
        case .connecting(let attempt):
            if attempt == 0, case .connecting = linkState { return }
            if let capabilities = linkState.cachedCapabilities {
                becomeInterrupted(.interrupted(snapshot.state, capabilities))
            } else if !linkState.isUsable, linkState != .connecting {
                linkState = .connecting
            }
        case .backoff:
            if linkState.cachedCapabilities != nil || linkState.isUsable {
                becomeInterrupted(.interrupted(snapshot.state, linkState.cachedCapabilities))
            } else {
                linkState = .interrupted(snapshot.state, nil)
            }
        case .macOffline:
            becomeInterrupted(.macOffline(linkState.cachedCapabilities))
        case .suspended:
            becomeInterrupted(.interrupted(.suspended, linkState.cachedCapabilities))
        case .revoked(let reason):
            becomeInterrupted(.revoked(reason))
            detachAllTerminals()
        case .pairingRequired(let reason):
            becomeInterrupted(.pairingRequired(reason))
            detachAllTerminals()
        case .disabled:
            break
        }
    }

    /// The link is not usable. Keep what was on screen, mark it stale, stop
    /// the sockets that cannot survive, and turn held terminals into
    /// interrupted ones (never silently closed, never replayed).
    func becomeInterrupted(_ state: LinkState) {
        if linkState.isUsable {
            sessionsStale = !sessions.isEmpty
            pathReporter.clear()
            conversationStores.values.forEach { $0.stop() }
            interruptTerminals()
        }
        linkState = state
    }

    /// Discovery once per authenticated link. Capabilities may have changed
    /// while the phone was away, and a restarted gateway instance re-bases
    /// every conversation socket.
    func discoverOnFreshLink() async {
        LinkTrace.shared.mark("app.discovery.begin")
        defer { LinkTrace.shared.mark("app.discovery.end") }
        guard let gateway, pairedDevice != nil else { return }
        let generation = pairedConnectionGeneration
        do {
            let started = Date()
            let capabilities = try await gateway.discover()
            record(LinkStageSample(stage: .discovery, milliseconds: Self.millis(since: started)))
            guard generation == pairedConnectionGeneration else { return }
            let instanceChanged = gatewayInstanceID != nil && gatewayInstanceID != capabilities.gatewayInstanceId
            gatewayInstanceID = capabilities.gatewayInstanceId
            // Permission refreshes can arrive while discovery is in flight.
            // Keep the current record rather than restoring a pre-await grant.
            linkSource = .paired
            linkState = .linked(capabilities)
            if let timings = linkSnapshot.timings {
                record(LinkStageSample(
                    stage: .applicationReady,
                    milliseconds: timings.linkReadyMs + Self.millis(since: started)
                ))
            }
            conversationStores.values.forEach {
                $0.reconnect(using: gateway, operationRetentionSeconds: capabilities.operationRetentionSeconds)
            }
            if instanceChanged {
                // A new gateway process has no receipts from the old one in
                // memory beyond its journal; sockets already re-based above.
                highlightedSessionID = nil
            }
            resumeInterruptedTerminals()
            await refreshSessions()
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
        } catch {
            linkState = .failed(error.localizedDescription)
        }
    }

    /// The owner's health probe for a link that looks alive after a network
    /// change: one bounded discovery request.
    func probeGateway() async -> Bool {
        guard let gateway else { return false }
        return (try? await gateway.discover()) != nil
    }
}
