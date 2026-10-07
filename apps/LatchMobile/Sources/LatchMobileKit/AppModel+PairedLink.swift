import Foundation

// MARK: - PairedLink

extension AppModel {
    /// Forgets the computer and everything fetched from it.
    public func unlink() {
        coordinatorObservation?.cancel()
        coordinatorObservation = nil
        Task { [coordinator] in await coordinator.stop() }
        transport?.stop()
        transport = nil
        link = nil
        gateway = nil
        linkSource = nil
        pairedDevice = nil
        pairedConnectionGeneration &+= 1
        discoveredGeneration = 0
        gatewayInstanceID = nil
        sessions = []
        sessionsStale = false
        sessionsError = nil
        highlightedSessionID = nil
        stoppingSessionIDs = []
        conversationStores.values.forEach { $0.stop() }
        conversationStores = [:]
        detachAllTerminals()
        pathReporter.clear()
        linkState = .unlinked
    }

    /// Establishes the paired route: starts the one link owner for this
    /// record, builds the capability-protected adapter and gateway, and lets
    /// the owner's snapshots drive discovery and every later state.
    public func connectPairedDevice(_ record: PairedDeviceRecord?) async {
        guard let record, record.isActive else {
            if linkSource == .paired { unlink() }
            return
        }
        // Keep the paired identity even if the current network route cannot
        // be established. It is the authority to retry on foreground, while
        // the listener and Noise sockets themselves are strictly ephemeral.
        pairedDevice = record
        await settleSuspension()
        pairedConnectionGeneration &+= 1
        let generation = pairedConnectionGeneration
        linkState = .connecting
        await coordinator.setOptions(diagnosticsOptions)
        await coordinator.setProbe { [weak self] in
            await self?.probeGateway() ?? false
        }
        do {
            transport?.stop()
            let provider = CoordinatorChannelProvider(coordinator: coordinator)
            let recorder: LinkStageRecorder = { [weak self] sample in
                Task { @MainActor in self?.record(sample) }
            }
            let gateway = try await gatewayFactory(record, provider, recorder)
            guard generation == pairedConnectionGeneration else { return }
            self.gateway = gateway
            self.link = await gateway.gateway
            self.transport = nil
            linkSource = .paired
        } catch let error as LatchError {
            linkState = Self.linkFailure(error)
            return
        } catch let error as RemoteLinkTransportError {
            linkState = .failed(error.message)
            return
        } catch {
            linkState = .failed(error.localizedDescription)
            return
        }
        observeCoordinator()
        await coordinator.start(record: record)
        // Callers get a settled answer: linked, or a typed reason it is not.
        // Later changes keep arriving through the owner's snapshots.
        await waitUntilSettled()
    }

    /// Applies a permission-only refresh without rebuilding a healthy route.
    ///
    /// The host checks the current local grant on every request, so the saved
    /// record is only the phone UI's projection. Replacing that projection in
    /// place makes a Mac-side terminal toggle visible as soon as it is read
    /// from the control plane, while identity, endpoint, or revocation changes
    /// still take the full reconnect path.
    @discardableResult
    public func applyPairedDeviceRecord(_ record: PairedDeviceRecord?) -> Bool {
        guard linkSource == .paired,
              let current = pairedDevice,
              let record,
              record.isActive,
              current.updating(permission: record.permission) == record
        else { return false }
        pairedDevice = record
        // A downgrade during recovery closes what the lesser grant no longer
        // covers before anything is fetched on the reconnected link.
        if !record.permission.permits(.control) {
            detachAllTerminals()
        }
        return true
    }

    /// Repeats discovery on a usable link, or cuts a backoff short.
    ///
    /// The contract requires discovery before the app resumes application
    /// traffic on a reconnected path; that happens automatically per link
    /// generation. This is the person's "Check again".
    public func rediscover() async {
        guard pairedDevice != nil else { return }
        if linkState.isUsable {
            await discoverOnFreshLink()
        } else {
            await coordinator.retryImmediately()
        }
    }

    /// A real network path change. One immediate attempt through the same
    /// owner: a backoff is cut short, a live link is probed and replaced if
    /// the probe fails. No second retry loop.
    public func networkPathChanged() async {
        guard pairedDevice != nil else { return }
        await coordinator.retryImmediately()
    }

    /// Releases the route before the app is suspended.
    ///
    /// The loopback adapter and its capability die here, the native link is
    /// closed, and conversation sockets stop. The paired record remains so
    /// `resumeAfterSuspension` can make a fresh adapter with a fresh
    /// capability and repeat discovery on foreground.
    public func suspendPairedTransport() {
        guard pairedDevice != nil else { return }
        LinkTrace.shared.mark("app.suspend")
        pairedConnectionGeneration &+= 1
        let coordinator = self.coordinator
        let previous = pendingSuspension
        pendingSuspension = Task {
            await previous?.value
            await coordinator.suspend()
        }
        transport?.stop()
        transport = nil
        if let gateway {
            Task { await gateway.stopTransport() }
        }
        gateway = nil
        link = nil
        highlightedSessionID = nil
        pathReporter.clear()
        becomeInterrupted(.interrupted(.suspended, linkState.cachedCapabilities))
    }

    /// Re-establishes the route after suspension: a new adapter and
    /// capability, then the same owner resumes and discovery follows.
    public func reconnectPairedTransport() async {
        guard let pairedDevice else { return }
        await settleSuspension()
        pairedConnectionGeneration &+= 1
        let generation = pairedConnectionGeneration
        do {
            let provider = CoordinatorChannelProvider(coordinator: coordinator)
            let recorder: LinkStageRecorder = { [weak self] sample in
                Task { @MainActor in self?.record(sample) }
            }
            let gateway = try await gatewayFactory(pairedDevice, provider, recorder)
            guard generation == pairedConnectionGeneration else { return }
            self.gateway = gateway
            self.link = await gateway.gateway
            linkSource = .paired
        } catch {
            linkState = .failed(error.localizedDescription)
            return
        }
        if coordinatorObservation == nil { observeCoordinator() }
        await coordinator.resume()
    }

    /// Restores a usable route and repeats discovery before any conversation
    /// socket is allowed to resume application traffic.
    public func resumeAfterSuspension() async {
        guard pairedDevice != nil else { return }
        LinkTrace.shared.mark("app.resume.begin")
        defer { LinkTrace.shared.mark("app.resume.end") }
        await settleSuspension()
        LinkTrace.shared.mark("app.resume.suspensionSettled")
        if gateway == nil {
            await reconnectPairedTransport()
        } else {
            await coordinator.retryImmediately()
        }
        // Discovery runs when the owner reports the link ready; conversation
        // sockets resume only once that produced a usable gateway.
        await waitUntilSettled()
        guard case .linked = linkState else { return }
        resumeConversations()
    }

    /// Lets an in-flight suspension finish before the owner is resumed, so
    /// suspend and resume always apply in the order they were requested.
    private func settleSuspension() async {
        if let pendingSuspension {
            await pendingSuspension.value
            if self.pendingSuspension == pendingSuspension { self.pendingSuspension = nil }
        }
    }

    /// Waits for the current attempt to reach a usable or non-retrying state,
    /// bounded so a caller never hangs on a Mac that is offline.
    public func waitUntilSettled(timeout: Duration = .seconds(20)) async {
        let deadline = Date().addingTimeInterval(
            Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
        )
        while Date() < deadline {
            switch linkState {
            case .linked, .revoked, .pairingRequired, .incompatible, .failed, .macOffline, .unlinked:
                return
            case .connecting, .interrupted:
                try? await Task.sleep(for: .milliseconds(25))
            }
        }
    }
}
