import Foundation
import AppKit

/// The user-controlled Remote Access lifecycle.
///
/// Remote access is off until the user turns it on, and turning it off is the
/// incident switch: the helper is stopped, pending pairing material is
/// cancelled, and the supervised gateway credential is removed. Nothing here
/// starts on launch, and no state is restored implicitly — `restoreIfEnabled()`
/// only resumes supervision when the CLI already reports remote access as on.
@MainActor
final class RemoteAccessController: ObservableObject {
    @Published private(set) var status: RemoteAccessStatus = .unavailable
    @Published private(set) var phase: RemoteAccessPhase = .off
    @Published private(set) var devices: [RemoteDevice] = []
    @Published private(set) var auditEvents: [RemoteAuditEvent] = []
    @Published private(set) var isBusy = false
    @Published var errorMessage: String?
    /// One-time pairing material, held only while the sheet is up.
    @Published var pendingPairing: PairingMaterial?
    /// What the open pairing sheet is waiting for.
    @Published private(set) var pairingProgress: RemotePairingProgress = .idle
    /// True from the moment "Pair a Device" is pressed until a code is on
    /// screen or the attempt has failed. Creating a code enrolls this Mac and
    /// registers the code with the control plane, which is a network round
    /// trip: without this the button looks dead for as long as that takes.
    @Published private(set) var isPairing = false
    /// Why the last attempt to create a code failed.
    ///
    /// Separate from `errorMessage` because this one is raised in front of the
    /// person. The general error row sits at the bottom of a long settings
    /// form, below the fold, where a failure that produced no code at all
    /// reads as a button that does nothing.
    @Published var pairingFailure: String?
    /// The control-plane address as it should appear in the settings field.
    @Published var controlPlaneAddress: String = ""

    /// Restart backoff for a helper that keeps dying. Capped so a permanently
    /// broken CLI cannot become a spin loop.
    private static let restartDelays: [UInt64] = [1, 2, 5, 10, 30]
    private static let readinessAttempts = 40
    private static let readinessInterval: UInt64 = 250_000_000

    /// How often the open pairing sheet asks the control plane whether the
    /// phone has enrolled yet, and how long it keeps asking. The window is the
    /// life of the code itself: once it expires there is nothing to wait for.
    private static let enrollmentPollInterval: UInt64 = 2_000_000_000
    private static let enrollmentGrace: TimeInterval = 15

    private let client: LatchClient
    private let controlPlane: ControlPlaneHost
    private var supervision: Task<Void, Never>?
    private var enrollmentWatch: Task<Void, Never>?
    /// Publishes the helper's authenticated listener, never the loopback
    /// gateway. It is separate from supervision because a healthy helper can
    /// temporarily have no listener to advertise.
    private var presenceTask: Task<Void, Never>?
    /// Offers that passed a fresh local device-state check. They are kept only
    /// in memory for the transport layer that will consume them; a control
    /// plane offer is never enough to authorize the local gateway.
    private(set) var approvedRendezvousOffers: [ControlPlaneRendezvousOffer] = []
    /// The ICE credentials presence advertises. They belong to the agent that
    /// answers connectivity checks — the helper — so the helper mints them once
    /// per run and reports them through the readiness document this app already
    /// polls. Generating them here would advertise a ufrag no agent recognises
    /// and every connectivity check would fail with 401.
    var iceCredentials: ControlPlaneIceCredentials? {
        status.ice.map { ControlPlaneIceCredentials(ufrag: $0.ufrag, pwd: $0.password) }
    }
    /// Offers already handed to the helper. The control plane's queue is
    /// one-shot, but presence refreshes faster than an ICE attempt completes,
    /// so this stops a still-in-flight request being answered twice.
    private var deliveredOffers: Set<String> = []
    private var terminationObserver: NSObjectProtocol?
    /// Keeps the Mac out of idle sleep while a phone is connected to it.
    private let sleepAssertion: SleepAssertionHolder
    /// Watches the connection count on its own clock. The general status poll
    /// backs off to a minute once Latch is not the app in front — which is
    /// exactly the state a Mac falls asleep from, so the assertion cannot wait
    /// for it. This asks for status alone, not the device list or the audit
    /// trail, so the extra cadence is one short-lived read.
    private var sleepWatch: Task<Void, Never>?
    private static let sleepPollInterval: UInt64 = 10_000_000_000

    convenience init() {
        self.init(client: LatchClient(), controlPlane: ControlPlaneHost())
    }

    init(
        client: LatchClient,
        controlPlane: ControlPlaneHost,
        sleepAssertion: SleepAssertionHolder = SleepAssertionHolder()
    ) {
        self.client = client
        self.controlPlane = controlPlane
        self.sleepAssertion = sleepAssertion
        self.controlPlaneAddress = controlPlane.address?.absoluteString ?? ""
    }

    /// Whether this Mac is currently being kept awake for a connected phone.
    var isPreventingSleep: Bool { sleepAssertion.isHeld }

    var isEnabled: Bool { status.enabled }

    var activeDevices: [RemoteDevice] { devices.filter { !$0.revoked } }
    var revokedDevices: [RemoteDevice] { devices.filter(\.revoked) }

    /// Only security-relevant rows are surfaced by default; the raw trail stays
    /// available for export via diagnostics.
    var securityEvents: [RemoteAuditEvent] {
        Array(auditEvents.filter(\.isSecurityRelevant).suffix(50).reversed())
    }

    var connectionEvents: [RemoteAuditEvent] {
        Array(
            auditEvents
                .filter { $0.event.hasPrefix("connection_") || $0.event.hasPrefix("lan_") }
                .suffix(50)
                .reversed()
        )
    }

    // MARK: - Lifecycle

    /// Reads the CLI state and, if the user previously left remote access on,
    /// resumes supervision. Called once when Settings first appears.
    ///
    /// Ongoing status polls ride SessionStore's companion refresh rather than
    /// a second 5-second timer of their own.
    func restoreIfEnabled() async {
        installTerminationHandler()
        await refresh()
        if status.enabled, supervision == nil {
            startSupervision()
        }
    }

    func setEnabled(_ enabled: Bool) async {
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            if enabled {
                try await client.enableRemoteAccess()
                await refresh()
                startSupervision()
                await waitForListener()
            } else {
                stopPresence(clear: true)
                stopSupervision()
                try await client.disableRemoteAccess()
                phase = .off
                dismissPairing()
                await refresh()
            }
            errorMessage = nil
        } catch {
            // A failed enable must not leave a helper running behind a UI that
            // says remote access is off.
            stopSupervision()
            stopPresence(clear: true)
            phase = .failed(error.localizedDescription)
            errorMessage = error.localizedDescription
            await refresh()
        }
    }

    private func startSupervision() {
        guard supervision == nil else { return }
        startSleepWatch()
        phase = .starting
        let executableURL = client.executableURL
        supervision = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
                // A restarted helper is a new agent with new credentials. It
                // mints them itself and reports them through readiness, so
                // there is nothing to reset here.
                let supervisor = RemoteAccessSupervisor(executableURL: executableURL)
                RemoteAccessController.supervisorRegistry.register(supervisor)
                defer { RemoteAccessController.supervisorRegistry.remove(supervisor) }
                do {
                    try await supervisor.run()
                    if Task.isCancelled { return }
                    // A clean exit that we did not ask for is still a stop.
                    attempt = min(attempt + 1, Self.restartDelays.count - 1)
                } catch {
                    if Task.isCancelled { return }
                    self?.recordHelperFailure(error)
                    attempt = min(attempt + 1, Self.restartDelays.count - 1)
                }
                let delay = Self.restartDelays[attempt]
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            }
        }
    }

    private func stopSupervision() {
        supervision?.cancel()
        supervision = nil
        sleepWatch?.cancel()
        sleepWatch = nil
        // No helper, no connections. Dropped here rather than waiting for the
        // next status poll, so turning remote access off releases the Mac at
        // the moment the person asked for it.
        sleepAssertion.apply(false)
        // No helper, no agent, and no readiness document: `iceCredentials`
        // reads as nil from the next status poll on its own.
        // Cancelling the task is not enough on its own: the child is only
        // reaped once it is asked to terminate.
        RemoteAccessController.terminateHelpers()
    }

    /// Matches the sleep assertion to the helper's connection count.
    ///
    /// A read that fails changes nothing: a CLI that could not be asked has not
    /// said the phone went away, and dropping the assertion on a transient
    /// failure would put the Mac to sleep underneath a live terminal. The next
    /// successful read — or `stopSupervision`, which is the person turning this
    /// off — releases it.
    private func startSleepWatch() {
        guard sleepWatch == nil else { return }
        sleepWatch = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if let current = try? await self.client.remoteAccessStatus() {
                    self.sleepAssertion.apply(current.hasLiveConnection)
                }
                try? await Task.sleep(nanoseconds: Self.sleepPollInterval)
            }
        }
    }

    private func recordHelperFailure(_ error: Error) {
        phase = .failed(error.localizedDescription)
        errorMessage = error.localizedDescription
    }

    /// Polls status until the helper advertises its authenticated listener.
    private func waitForListener() async {
        for _ in 0..<Self.readinessAttempts {
            await refresh()
            if let listener = status.listenerAddress {
                phase = .online(listener: listener)
                return
            }
            if case .failed = phase { return }
            try? await Task.sleep(nanoseconds: Self.readinessInterval)
        }
        phase = .failed(RemoteAccessSupervisorError.readinessTimeout.localizedDescription)
        errorMessage = RemoteAccessSupervisorError.readinessTimeout.localizedDescription
    }

    /// Starts a refresh loop only while a real non-loopback listener exists.
    /// The control plane returns its own TTL, so the loop wakes at a third of
    /// that window rather than baking an environment-specific lifetime into
    /// the desktop app.
    private func startPresence() {
        guard presenceTask == nil, status.enabled, status.listenerAddress != nil,
              controlPlane.isConfigured else { return }
        presenceTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let ttl = await self.publishPresenceAndCollectOffers()
                let delay = max(1, (ttl ?? 15) / 3)
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            }
        }
    }

    private func stopPresence(clear: Bool) {
        presenceTask?.cancel()
        presenceTask = nil
        approvedRendezvousOffers = []
        deliveredOffers = []
        guard clear else { return }
        Task { [controlPlane] in await controlPlane.clearPresence() }
    }

    /// Publishing and collection are deliberately adjacent: an offer reaches
    /// the future transport only after a fresh local device-state check. That
    /// avoids treating a still-valid control-plane offer as authorization after
    /// this Mac revoked the phone; established streams retain the CLI's 250ms
    /// device-state check.
    private func publishPresenceAndCollectOffers() async -> UInt64? {
        guard status.enabled, let listener = status.listenerAddress,
              let publicKey = status.publicKey, controlPlane.isConfigured else {
            return nil
        }
        // Presence advertises what the helper's agent actually gathered — its
        // host and server-reflexive candidates, tunnel interfaces included —
        // rather than only the LAN listener, which no phone off this network
        // can reach. A helper without an agent publishes no ICE at all instead
        // of credentials nothing will answer.
        let ice = iceCredentials
        let gathered = status.ice?.candidates ?? []
        let agentCandidates = Self.presenceCandidates(from: gathered, neverRelay: status.neverRelay)
            .map(\.published)
        do {
            let presence = try await controlPlane.publishPresence(
                publicKey: publicKey,
                macName: Self.macName,
                listenerAddress: listener,
                interfaceHosts: Self.interfaceHosts(from: gathered),
                ice: ice,
                agentCandidates: agentCandidates
            )
            let locallyAuthorized = Set(devices.filter { !$0.revoked }.map(\.deviceID))
            let offers = try await controlPlane.rendezvousOffers()
            approvedRendezvousOffers = offers.filter { locallyAuthorized.contains($0.peerDeviceID) }
            deliverApprovedOffers()
            return presence.ttlSeconds
        } catch {
            // Presence expiry is safe-fail: the Mac becomes unavailable rather
            // than keeping a stale route. Preserve the error for Settings and
            // retry on a short bounded cadence.
            errorMessage = error.localizedDescription
            return nil
        }
    }

    /// Chooses which gathered candidates presence carries.
    ///
    /// Presence takes at most eight, and the LAN listener already holds one of
    /// them. A Mac with several interfaces — physical, a VPN or tailnet tunnel,
    /// IPv6 for each — gathers more than that, so something has to be dropped.
    /// It must not be the server-reflexive candidates: they are the only ones a
    /// phone off this network can use at all, and ICE ranks them *below* host
    /// candidates precisely because it assumes both ends see the whole list.
    /// Host candidates fill whatever is left, highest priority first, so the
    /// LAN and tailnet paths keep the places they earn.
    /// - Parameter neverRelay: when the Mac refuses the relay outright, the
    ///   server-reflexive candidates are withheld as well. They exist to find a
    ///   path through the internet when neither end is on the other's network,
    ///   which is the situation that ends in a relay; a Mac that has said never
    ///   should publish only addresses that are reachable without one — its LAN
    ///   and its tailnet.
    static func presenceCandidates(
        from gathered: [RemoteIceCandidate],
        neverRelay: Bool = false
    ) -> [RemoteIceCandidate] {
        let gathered = neverRelay ? gathered.filter { $0.type == "host" } : gathered
        let ordered = gathered.enumerated().sorted { left, right in
            let leftIsHost = left.element.type == "host"
            let rightIsHost = right.element.type == "host"
            if leftIsHost != rightIsHost { return rightIsHost }
            if left.element.priority != right.element.priority {
                return left.element.priority > right.element.priority
            }
            // Gathering order is stable, so ties keep it rather than shuffling
            // the published list between presence refreshes.
            return left.offset < right.offset
        }
        return ordered
            .map(\.element)
            .prefix(ControlPlaneHost.maxCandidates - ControlPlaneHost.maxListenerCandidates)
            .map { $0 }
    }

    /// The interface addresses the authenticated TCP listener is reachable at.
    ///
    /// Taken from the agent's gathered host candidates, which is the one place
    /// this app learns which interfaces exist — tunnel interfaces included,
    /// which is the whole point. Only the address is kept: the listener has its
    /// own port, and the agent's UDP port would reach nothing over TCP.
    static func interfaceHosts(from gathered: [RemoteIceCandidate]) -> [String] {
        var seen: Set<String> = []
        return gathered
            .filter { $0.type == "host" }
            .compactMap { hostComponent(of: $0.address) }
            .filter { seen.insert($0).inserted }
    }

    /// The address half of a candidate's `host:port`, IPv6 brackets removed.
    static func hostComponent(of address: String) -> String? {
        if address.hasPrefix("[") {
            guard let close = address.firstIndex(of: "]") else { return nil }
            return String(address[address.index(after: address.startIndex)..<close])
        }
        guard let colon = address.lastIndex(of: ":") else { return nil }
        return String(address[..<colon])
    }

    /// Hands every newly approved offer to the helper's ICE agent.
    ///
    /// A failure here is not a presence failure: the phone can still reach this
    /// Mac on the LAN, and its next rendezvous request produces a fresh offer.
    /// It is recorded rather than raised in front of the person.
    private func deliverApprovedOffers() {
        let pending = approvedRendezvousOffers.filter { !deliveredOffers.contains($0.requestID) }
        guard !pending.isEmpty else { return }
        // Expired request ids are forgotten so this set cannot grow with the
        // life of the app.
        let now = UInt64(Date().timeIntervalSince1970)
        deliveredOffers = deliveredOffers.intersection(
            approvedRendezvousOffers.filter { $0.expiresAt > now }.map(\.requestID)
        )
        for offer in pending {
            deliveredOffers.insert(offer.requestID)
            guard let document = RemoteRendezvousOfferDocument(offer) else { continue }
            Task { [client] in
                do {
                    try await client.recordRendezvousOffer(document)
                } catch {
                    await MainActor.run { self.errorMessage = error.localizedDescription }
                }
            }
        }
    }

    private func installTerminationHandler() {
        guard terminationObserver == nil else { return }
        terminationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { _ in
            RemoteAccessController.terminateHelpers()
            Task { @MainActor [weak self] in
                self?.stopPresence(clear: true)
                self?.sleepAssertion.apply(false)
            }
        }
    }

    /// Terminates every helper this app launched. The helper then takes its
    /// supervised gateway down with it, so no plaintext listener outlives the
    /// app or an explicit disable.
    nonisolated static func terminateHelpers() {
        for supervisor in supervisorRegistry.drain() {
            supervisor.stop()
        }
    }

    fileprivate nonisolated static let supervisorRegistry = SupervisorRegistry()

    // MARK: - State

    func refresh() async {
        do {
            let nextStatus = try await client.remoteAccessStatus()
            if status != nextStatus {
                status = nextStatus
            }
            let nextDevices = try await client.remoteDevices()
            if devices != nextDevices {
                devices = nextDevices
            }
            if let nextAudit = try? await client.remoteAudit(), nextAudit != auditEvents {
                auditEvents = nextAudit
            }
            // Recomputed from status rather than driven by connection events:
            // a missed close cannot then leave a Mac awake indefinitely, since
            // the next poll that reports no connections drops the assertion.
            sleepAssertion.apply(status.hasLiveConnection)
            if !status.enabled {
                assignPhase(.off)
                stopPresence(clear: true)
            } else if let listener = status.listenerAddress {
                assignPhase(.online(listener: listener))
                startPresence()
            } else if case .failed = phase {
                // Keep the failure visible rather than downgrading it.
                stopPresence(clear: true)
            } else if supervision != nil {
                assignPhase(.starting)
                stopPresence(clear: true)
            }
        } catch {
            if errorMessage != error.localizedDescription {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func assignPhase(_ next: RemoteAccessPhase) {
        if phase != next {
            phase = next
        }
    }

    // MARK: - Control plane

    /// Saves the address typed in settings.
    ///
    /// Changing it deliberately forgets this Mac's credentials: tokens issued
    /// by one deployment name nothing in another, so carrying them across
    /// would only produce a rejected pairing later.
    func saveControlPlaneAddress() {
        let previous = controlPlane.address?.absoluteString
        do {
            try controlPlane.setAddress(controlPlaneAddress)
            let current = controlPlane.address?.absoluteString
            if current != previous {
                try controlPlane.forgetEnrollment()
            }
            controlPlaneAddress = current ?? ""
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    var isControlPlaneConfigured: Bool { controlPlane.isConfigured }

    // MARK: - Pairing

    /// Creates one code and, when a control plane is configured, registers it
    /// so the phone that scans it has somewhere to enroll.
    ///
    /// Registration failing is not silent: a code that was not registered
    /// cannot be completed by a phone, and showing it anyway would send the
    /// person to a scanner that can only fail.
    func createPairing() async {
        guard !isPairing else { return }
        guard status.enabled else {
            // The button is disabled in this state, so reaching here means the
            // status went stale. Saying so beats returning in silence.
            pairingFailure = "Remote access is off, so there is nothing for a phone to pair with. Turn it on first."
            return
        }
        enrollmentWatch?.cancel()
        enrollmentWatch = nil
        isPairing = true
        defer { isPairing = false }
        do {
            let material = try await client.createRemotePairing()
            guard controlPlane.isConfigured else {
                // A Mac with no control plane still pairs, but the phone has
                // to be told the address by hand.
                pendingPairing = material
                pairingProgress = .unaddressed
                errorMessage = nil
                pairingFailure = nil
                await refresh()
                return
            }
            guard let publicKey = status.publicKey else {
                throw ControlPlaneHostError.noIdentity
            }
            // Snapshot the directory before the code is registered, not after:
            // a phone that enrolls between the two would otherwise be counted
            // as already known and never noticed.
            let known = Set(((try? await controlPlane.pairedClients()) ?? []).map(\.deviceID))
            let addressed = try await controlPlane.openPairing(
                material,
                publicKey: publicKey,
                macName: Self.macName
            )
            pendingPairing = addressed
            pairingProgress = .waiting
            errorMessage = nil
            pairingFailure = nil
            await refresh()
            watchForEnrollment(addressed, known: known)
        } catch {
            // No code is shown, because a code that was not registered cannot
            // be completed by a phone. The reason is raised instead, so the
            // press always produces an answer.
            pairingProgress = .idle
            errorMessage = error.localizedDescription
            pairingFailure = error.localizedDescription
        }
    }

    func dismissPairing() {
        enrollmentWatch?.cancel()
        enrollmentWatch = nil
        pendingPairing = nil
        pairingProgress = .idle
    }

    /// Watches for the phone to appear in the control-plane directory, then
    /// records it locally.
    ///
    /// The control plane holds the directory; this Mac holds the
    /// authorization. Until the local `pair confirm` runs, the phone has an
    /// account row and no way through the authenticated transport, so this
    /// step is what actually completes pairing.
    private func watchForEnrollment(_ material: PairingMaterial, known: Set<String>) {
        enrollmentWatch = Task { [weak self] in
            guard let self else { return }
            let deadline = material.expiryDate.addingTimeInterval(Self.enrollmentGrace)
            while !Task.isCancelled, Date() < deadline {
                try? await Task.sleep(nanoseconds: Self.enrollmentPollInterval)
                guard !Task.isCancelled else { return }
                guard let enrolled = try? await self.controlPlane.pairedClients() else { continue }
                guard let phone = enrolled.first(where: { !known.contains($0.deviceID) }) else {
                    continue
                }
                await self.completeEnrollment(of: phone, for: material)
                return
            }
        }
    }

    private func completeEnrollment(of phone: ControlPlaneDevice, for material: PairingMaterial) async {
        do {
            let confirmation = try await client.confirmRemotePairing(
                pairingID: material.pairingID,
                secret: material.secret,
                devicePublicKey: phone.publicKey,
                name: phone.name,
                permission: phone.permission ?? .interact,
                controlPlaneDeviceID: phone.deviceID
            )
            pairingProgress = .enrolled(name: confirmation.name, phrase: confirmation.pairingPhrase)
            errorMessage = nil
            await refresh()
        } catch {
            pairingProgress = .failed(error.localizedDescription)
        }
    }

    /// What this Mac calls itself in a phone's device list.
    private static var macName: String {
        Host.current().localizedName ?? ProcessInfo.processInfo.hostName
    }

    // MARK: - Devices

    func grant(_ device: RemoteDevice, permission: DevicePermission) async {
        guard permission != device.permission else { return }
        do {
            // The local store is written first and on its own. It is what the
            // helper enforces, so a grant is in force — or withdrawn — before
            // any network call is attempted, and a control plane that cannot
            // be reached never leaves this Mac allowing more than the person
            // just asked for.
            try await client.grantRemoteDevice(device.deviceID, permission: permission)
            errorMessage = nil
            await refresh()
            await mirrorGrant(device, permission: permission)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Convenience for the Settings row: the terminal is carried by `control`
    /// and by nothing below it, so allowing it means granting `control` and
    /// withdrawing it means dropping back to what the device had otherwise.
    func setTerminalAllowed(_ allowed: Bool, for device: RemoteDevice) async {
        await grant(device, permission: allowed ? .control : device.permissionWithoutTerminal)
    }

    /// Restates the new permission in the control-plane directory the phone
    /// reads its own grant from.
    ///
    /// A failure here is reported but never rolls the local change back: the
    /// Mac's answer is the one that decides what a request is allowed to do,
    /// and the directory is a convenience for the phone's UI.
    private func mirrorGrant(_ device: RemoteDevice, permission: DevicePermission) async {
        guard isControlPlaneConfigured, let directoryID = device.controlPlaneDeviceID else { return }
        do {
            try await controlPlane.mirrorPermission(
                clientDeviceID: directoryID,
                permission: permission
            )
        } catch {
            errorMessage =
                "\(device.name) now has \(permission.label) on this Mac, but the change could not be sent to the control plane. The phone may still show its old permission until it reconnects."
        }
    }

    func revoke(_ device: RemoteDevice) async {
        do {
            try await client.revokeRemoteDevice(device.deviceID)
            errorMessage = nil
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// The strict refusal: no relay admission, and presence narrowed to
    /// addresses a phone can reach without one.
    ///
    /// It is the same account-level kill switch underneath, so a control plane
    /// that cannot be reached still leaves this Mac protected — the local
    /// refusal is written first, exactly as `setRelayEnabled(false)` does.
    func setNeverRelay(_ never: Bool) async {
        do {
            try await client.setRemoteNeverRelay(never)
            try await controlPlane.setRelayEnabled(false)
            errorMessage = nil
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
            await refresh()
        }
    }

    func setRelayEnabled(_ enabled: Bool) async {
        do {
            if enabled {
                try await controlPlane.setRelayEnabled(true)
                try await client.setRemoteRelayEnabled(true)
            } else {
                // Drop local relay admission first. If the account update
                // cannot be reached, this Mac is still protected and Settings
                // reports that the hosted policy needs attention.
                try await client.setRemoteRelayEnabled(false)
                try await controlPlane.setRelayEnabled(false)
            }
            errorMessage = nil
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Writes the content-free diagnostics bundle the runbook asks for. It is
    /// never uploaded; the user chooses where it lands.
    func exportDiagnostics(to url: URL) async {
        do {
            let bundle = try await client.remoteDiagnostics()
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(bundle).write(to: url, options: .atomic)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// Tracks live helpers so app termination can stop every one of them, including
/// a helper whose supervision task was already replaced by a restart.
final class SupervisorRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var supervisors: [RemoteAccessSupervisor] = []

    func register(_ supervisor: RemoteAccessSupervisor) {
        lock.lock()
        defer { lock.unlock() }
        supervisors.append(supervisor)
    }

    func remove(_ supervisor: RemoteAccessSupervisor) {
        lock.lock()
        defer { lock.unlock() }
        supervisors.removeAll { $0 === supervisor }
    }

    func drain() -> [RemoteAccessSupervisor] {
        lock.lock()
        defer { lock.unlock() }
        let current = supervisors
        supervisors.removeAll()
        return current
    }
}
