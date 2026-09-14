import AppKit
import Foundation

/// Owns every Desktop-launched Remote Link helper and the explicit enrollment
/// decision. The ordinary `latch` command remains a local grant authority and
/// fixed loopback proxy; it never opens an internet transport.
@MainActor
final class RemoteAccessController: ObservableObject {
    @Published private(set) var status: RemoteAccessStatus = .unavailable
    @Published private(set) var phase: RemoteAccessPhase = .off
    @Published private(set) var devices: [RemoteDevice] = []
    @Published private(set) var auditEvents: [RemoteAuditEvent] = []
    @Published private(set) var isBusy = false
    @Published var errorMessage: String?
    @Published var pendingPairing: RemoteEnrollmentMaterial?
    @Published private(set) var pairingProgress: RemotePairingProgress = .idle
    @Published private(set) var isPairing = false
    @Published var pairingFailure: String?
    @Published var controlPlaneAddress = ""
    @Published var ownerInvitation = ""
    /// Per-helper link status, keyed by the phone's directory id.
    @Published private(set) var linkStatuses: [String: HelperLinkStatus] = [:]
    /// Opt-in: keep this Mac awake while a phone is connected and the Mac is
    /// on external power. Persisted; off by default.
    @Published var keepAwakeWhilePluggedIn: Bool {
        didSet {
            defaults.set(keepAwakeWhilePluggedIn, forKey: Self.keepAwakeKey)
            applySleepPolicy()
        }
    }
    @Published private(set) var isOnExternalPower = false
    @Published private(set) var isPreventingSleep = false

    static let keepAwakeKey = "remoteAccessKeepAwakeWhilePluggedIn"
    private static let restartDelays: [UInt64] = [1, 2, 5, 10, 30]
    /// A helper that stayed up this long before exiting was not crash-looping;
    /// its next restart starts the schedule over. Without this, every helper
    /// exit over the app's lifetime climbed the schedule and a tenth restart
    /// waited the full 30 s (seen in the field on 8 September 2026).
    static let healthyHelperUptime: TimeInterval = 60

    static func nextRestartAttempt(after uptime: TimeInterval, previous: Int) -> Int {
        uptime >= healthyHelperUptime ? 0 : min(previous + 1, restartDelays.count - 1)
    }
    private static let powerPollInterval: Duration = .seconds(30)
    /// Directory re-check while no phone is linked.
    static let idleLinkPollInterval: Duration = .seconds(20)
    private let client: LatchClient
    private let controlPlane: ControlPlaneHost
    private let defaults: UserDefaults
    private let sleepAssertion: SleepAssertionHolder
    private let powerSource: any PowerSourceObserving
    private var powerWatch: Task<Void, Never>?
    private var attention: Task<Void, Never>?
    private var supervision: Task<Void, Never>?
    private var gatewaySupervision: Task<Void, Never>?
    private var gatewaySupervisor: RemoteGatewaySupervisor?
    private var gatewayReady = false
    private var activeAssignments: [String: RemoteLinkAssignment] = [:]
    private var linkSupervisions: [String: Task<Void, Never>] = [:]
    private var linkSupervisors: [String: RemoteAccessSupervisor] = [:]
    private var linkGenerations: [String: UUID] = [:]
    private var enrollmentWatch: Task<Void, Never>?
    private var enrollmentHelper: RemoteEnrollmentSupervisor?
    private var enrollmentDecision: CheckedContinuation<Bool, Never>?
    private var pendingPhoneName: String?
    private var terminationObserver: NSObjectProtocol?

    convenience init() { self.init(client: LatchClient(), controlPlane: ControlPlaneHost()) }

    init(
        client: LatchClient,
        controlPlane: ControlPlaneHost,
        sleepAssertion: SleepAssertionHolder = SleepAssertionHolder(),
        powerSource: any PowerSourceObserving = IOPSPowerSource(),
        defaults: UserDefaults = LatchClient.preferences
    ) {
        self.client = client
        self.controlPlane = controlPlane
        self.sleepAssertion = sleepAssertion
        self.powerSource = powerSource
        self.defaults = defaults
        self.keepAwakeWhilePluggedIn = defaults.bool(forKey: Self.keepAwakeKey)
        self.controlPlaneAddress = controlPlane.address?.absoluteString ?? ""
        self.isOnExternalPower = powerSource.isOnExternalPower()
    }

    var isEnabled: Bool { status.enabled }
    /// Helpers whose link is authenticated and serving right now.
    var connectedPeers: Int { linkStatuses.values.filter(\.isConnected).count }
    var activeDevices: [RemoteDevice] { devices.filter { !$0.revoked } }
    var revokedDevices: [RemoteDevice] { devices.filter(\.revoked) }
    var isControlPlaneConfigured: Bool { controlPlane.isConfigured }

    var securityEvents: [RemoteAuditEvent] {
        Array(auditEvents.filter(\.isSecurityRelevant).suffix(50).reversed())
    }

    var connectionEvents: [RemoteAuditEvent] {
        Array(auditEvents.filter { $0.event.hasPrefix("connection_") || $0.event.hasPrefix("link_") }
            .suffix(50).reversed())
    }

    func restoreIfEnabled() async {
        installTerminationHandler()
        await refresh()
        if status.enabled { startSupervision() }
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
            } else {
                stopSupervision()
                dismissPairing()
                try await client.disableRemoteAccess()
                phase = .off
                await refresh()
            }
            errorMessage = nil
        } catch {
            stopSupervision()
            phase = .failed(error.localizedDescription)
            errorMessage = error.localizedDescription
            await refresh()
        }
    }

    private func startSupervision() {
        guard supervision == nil, gatewaySupervision == nil else { return }
        phase = .starting
        startAttentionForwarding()
        startPowerWatch()
        startAssignmentSupervision()
    }

    /// The gateway has its own lifecycle because it is shared infrastructure,
    /// not a child resource of any phone. A gateway crash is visible at the
    /// aggregate level and retried with bounded backoff without tearing down
    /// authenticated device links.
    private func startGatewaySupervision() {
        guard gatewaySupervision == nil else { return }
        let executableURL = client.executableURL
        gatewaySupervision = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
                guard let self else { return }
                let startedAt = Date()
                let supervisor = RemoteGatewaySupervisor(executableURL: executableURL)
                self.gatewaySupervisor = supervisor
                Self.supervisorRegistry.register(supervisor)
                do {
                    try await supervisor.run { [weak self] in
                        Task { @MainActor in self?.recordGatewayReady(from: supervisor) }
                    }
                } catch {
                    guard !Task.isCancelled else {
                        Self.supervisorRegistry.remove(supervisor)
                        return
                    }
                    self.gatewayReady = false
                    self.phase = .failed("Shared Conversation Hub gateway stopped: \(error.localizedDescription). Retrying…")
                }
                Self.supervisorRegistry.remove(supervisor)
                if self.gatewaySupervisor === supervisor { self.gatewaySupervisor = nil }
                guard !Task.isCancelled else { return }
                self.gatewayReady = false
                attempt = Self.nextRestartAttempt(
                    after: Date().timeIntervalSince(startedAt), previous: attempt
                )
                try? await Task.sleep(for: .seconds(Self.restartDelays[attempt]))
            }
        }
    }

    private func recordGatewayReady(from supervisor: RemoteGatewaySupervisor) {
        guard gatewaySupervisor === supervisor else { return }
        gatewayReady = true
        phase = .onlineRelay(peers: connectedPeers)
    }

    /// Discovers the desired assignments and reconciles only the links whose
    /// device identity or grant revision changed. Existing healthy links are
    /// never grouped under another device's failure.
    private func startAssignmentSupervision() {
        guard supervision == nil else { return }
        supervision = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
                guard let self else { return }
                var discovered = false
                do {
                    let assignments = try await self.remoteLinkAssignments()
                    guard !Task.isCancelled else { return }
                    self.reconcile(assignments)
                    attempt = 0
                    discovered = true
                    if self.gatewayReady { self.phase = .onlineRelay(peers: self.connectedPeers) }
                } catch {
                    guard !Task.isCancelled else { return }
                    self.errorMessage = error.localizedDescription
                    attempt = min(attempt + 1, Self.restartDelays.count - 1)
                }
                let delay: Duration
                if discovered, !activeAssignments.isEmpty {
                    // Assignment-changing owner actions cancel this task and
                    // reconcile immediately. Avoid continuously consuming
                    // otherwise-unused single-use admissions while links run.
                    delay = .seconds(24 * 60 * 60)
                } else if discovered {
                    delay = Self.idleLinkPollInterval
                } else {
                    delay = .seconds(Self.restartDelays[attempt])
                }
                try? await Task.sleep(for: delay)
            }
        }
    }

    private func reconcile(_ assignments: [RemoteLinkAssignment]) {
        let desired = Dictionary(uniqueKeysWithValues: assignments.map { ($0.peerDeviceID, $0) })
        if desired.isEmpty {
            stopGatewaySupervision()
            phase = .onlineRelay(peers: 0)
        } else {
            startGatewaySupervision()
        }
        for peerID in Array(activeAssignments.keys) {
            guard let current = activeAssignments[peerID] else { continue }
            guard let next = desired[peerID], current.hasSameAuthority(as: next) else {
                stopLink(peerID)
                continue
            }
        }
        for (peerID, assignment) in desired where activeAssignments[peerID] == nil {
            startLink(assignment)
        }
    }

    private func startLink(_ assignment: RemoteLinkAssignment) {
        let peerID = assignment.peerDeviceID
        activeAssignments[peerID] = assignment
        linkSupervisions[peerID] = Task { [weak self] in
            guard let self else { return }
            await self.superviseLink(assignment)
        }
    }

    private func superviseLink(_ assignment: RemoteLinkAssignment) async {
        let peerID = assignment.peerDeviceID
        var configuration = assignment.configuration
        var attempt = 0
        while !Task.isCancelled, activeAssignments[peerID]?.hasSameAuthority(as: assignment) == true {
            while !gatewayReady, !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
            }
            guard !Task.isCancelled else { return }
            let startedAt = Date()
            let generation = UUID()
            linkGenerations[peerID] = generation
            let supervisor = RemoteAccessSupervisor(
                executableURL: client.executableURL,
                configuration: configuration,
                renewLease: { [weak self] leaseID in
                    guard let self else { throw CancellationError() }
                    return try await self.controlPlane.renewRemoteLinkLease(leaseID)
                },
                requestAdmission: { [weak self] in
                    guard let self else { throw CancellationError() }
                    return try await self.controlPlane.freshRelayAdmission(peerDeviceID: peerID)
                },
                onStatus: { [weak self] status in
                    Task { @MainActor in
                        self?.recordLinkStatus(status, for: peerID, generation: generation)
                    }
                }
            )
            linkSupervisors[peerID] = supervisor
            Self.supervisorRegistry.register(supervisor)
            do {
                try await supervisor.run()
            } catch {
                guard !Task.isCancelled else {
                    Self.supervisorRegistry.remove(supervisor)
                    return
                }
                linkStatuses[peerID] = .offline
                if gatewayReady { phase = .onlineRelay(peers: connectedPeers) }
            }
            Self.supervisorRegistry.remove(supervisor)
            if linkSupervisors[peerID] === supervisor { linkSupervisors[peerID] = nil }
            guard !Task.isCancelled else { return }
            attempt = Self.nextRestartAttempt(
                after: Date().timeIntervalSince(startedAt), previous: attempt
            )
            try? await Task.sleep(for: .seconds(Self.restartDelays[attempt]))
            guard !Task.isCancelled else { return }
            while !Task.isCancelled {
                if let admission = try? await controlPlane.freshRelayAdmission(peerDeviceID: peerID) {
                    configuration = configuration.replacingAdmission(admission)
                    break
                }
                linkStatuses[peerID] = .offline
                try? await Task.sleep(for: .seconds(Self.restartDelays[attempt]))
            }
        }
    }

    private func stopLink(_ peerID: String) {
        linkSupervisors.removeValue(forKey: peerID)?.stop()
        linkSupervisions.removeValue(forKey: peerID)?.cancel()
        activeAssignments.removeValue(forKey: peerID)
        linkStatuses.removeValue(forKey: peerID)
        linkGenerations.removeValue(forKey: peerID)
    }

    private func stopGatewaySupervision() {
        gatewaySupervision?.cancel()
        gatewaySupervision = nil
        gatewaySupervisor?.stop()
        gatewaySupervisor = nil
        gatewayReady = false
    }

    private func remoteLinkAssignments() async throws -> [RemoteLinkAssignment] {
        guard controlPlane.isConfigured, let publicKey = status.publicKey else { return [] }
        return try await controlPlane.remoteLinkAssignments(
            publicKey: publicKey, macName: Self.macName, retaining: activeAssignments
        )
    }

    /// One helper reported where its link is. Connected peers drive both the
    /// status line and the keep-awake assertion.
    func recordLinkStatus(_ status: HelperLinkStatus, for peerDeviceID: String) {
        linkStatuses[peerDeviceID] = status
        if case .onlineRelay = phase {
            phase = .onlineRelay(peers: connectedPeers)
        }
        applySleepPolicy()
    }

    private func recordLinkStatus(
        _ status: HelperLinkStatus,
        for peerDeviceID: String,
        generation: UUID
    ) {
        guard linkGenerations[peerDeviceID] == generation else { return }
        recordLinkStatus(status, for: peerDeviceID)
    }

    /// Recomputes the sleep assertion from current facts. Idempotent, and
    /// re-run on a timer so a missed status or a power change cannot leave
    /// the Mac awake with nobody connected.
    func applySleepPolicy() {
        isOnExternalPower = powerSource.isOnExternalPower()
        let prevent = SleepPolicy.shouldPreventSleep(
            keepAwake: keepAwakeWhilePluggedIn,
            externalPower: isOnExternalPower,
            connectedPeers: connectedPeers
        )
        sleepAssertion.apply(prevent)
        isPreventingSleep = sleepAssertion.isHeld
    }

    private func startPowerWatch() {
        guard powerWatch == nil else { return }
        powerWatch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.powerPollInterval)
                guard !Task.isCancelled else { return }
                self?.applySleepPolicy()
            }
        }
    }

    private func startAttentionForwarding() {
        guard attention == nil else { return }
        let client = self.client
        let controlPlane = self.controlPlane
        let forwarder = AttentionForwarder(
            fetch: { try await client.remoteAttentionEvents() },
            acknowledge: { try await client.acknowledgeRemoteAttention($0) },
            directoryID: { [weak self] localID in
                guard let controller = self else { return nil }
                return await MainActor.run {
                    controller.devices.first { $0.deviceID == localID && !$0.revoked }?.controlPlaneDeviceID
                }
            },
            notify: { clientID, eventID in
                _ = try await controlPlane.notifyAttention(clientDeviceID: clientID, eventID: eventID)
            }
        )
        attention = Task { await forwarder.run() }
    }

    private func restartSupervision(clearLinks: Bool = false) {
        guard status.enabled else { return }
        if clearLinks {
            for peerID in Array(activeAssignments.keys) { stopLink(peerID) }
        }
        supervision?.cancel()
        supervision = nil
        startAssignmentSupervision()
    }

    private func stopSupervision() {
        supervision?.cancel()
        supervision = nil
        stopGatewaySupervision()
        for peerID in Array(activeAssignments.keys) { stopLink(peerID) }
        attention?.cancel()
        attention = nil
        powerWatch?.cancel()
        powerWatch = nil
        linkStatuses = [:]
        Self.terminateHelpers()
        applySleepPolicy()
    }

    nonisolated static func terminateHelpers() {
        supervisorRegistry.drain().forEach { $0.stop() }
    }

    fileprivate nonisolated static let supervisorRegistry = SupervisorRegistry()

    func refresh() async {
        do {
            let nextStatus = try await client.remoteAccessStatus()
            if status != nextStatus { status = nextStatus }
            let nextDevices = try await client.remoteDevices()
            if devices != nextDevices { devices = nextDevices }
            if let nextAudit = try? await client.remoteAudit(), auditEvents != nextAudit { auditEvents = nextAudit }
            if !status.enabled {
                phase = .off
                stopSupervision()
            } else if supervision == nil {
                phase = .starting
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func saveControlPlaneAddress() {
        let previous = controlPlane.address?.absoluteString
        do {
            try controlPlane.setAddress(controlPlaneAddress)
            let current = controlPlane.address?.absoluteString
            if current != previous {
                try controlPlane.forgetEnrollment()
                restartSupervision(clearLinks: true)
            }
            controlPlaneAddress = current ?? ""
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func saveOwnerInvitation() {
        do {
            try controlPlane.setOwnerInvitation(ownerInvitation)
            ownerInvitation = ""
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createPairing() async {
        guard !isPairing else { return }
        guard status.enabled else {
            pairingFailure = "Remote access is off. Turn it on before pairing a phone."
            return
        }
        enrollmentWatch?.cancel()
        isPairing = true
        defer { isPairing = false }
        do {
            guard let publicKey = status.publicKey else { throw ControlPlaneHostError.noIdentity }
            let material = try await controlPlane.openRemoteEnrollment(publicKey: publicKey, macName: Self.macName)
            let helper = RemoteEnrollmentSupervisor(
                executableURL: client.executableURL,
                configuration: RemoteLinkHostConfiguration(
                    purpose: "enrollment", relayUrl: material.relayURL,
                    admission: material.hostAdmission, enrollmentId: material.enrollmentID,
                    enrollmentSecret: material.enrollmentSecret
                )
            )
            enrollmentHelper = helper
            Self.supervisorRegistry.register(helper)
            pendingPairing = material
            pairingProgress = .waiting
            pairingFailure = nil
            errorMessage = nil
            enrollmentWatch = Task { [weak self, helper] in
                guard let self else { return }
                defer { Self.supervisorRegistry.remove(helper) }
                do {
                    try await helper.run(
                        decide: { [weak self] proposal in
                            guard let self else { return false }
                            return await self.awaitOwnerDecision(proposal)
                        },
                        mirror: { [weak self] enrollmentID, key, permission, revision in
                            guard let self else { throw CancellationError() }
                            return try await self.controlPlane.completeRemoteEnrollment(
                                enrollmentID: enrollmentID, controllerPublicKey: key,
                                permission: permission, grantRevision: revision
                            )
                        }
                    )
                    guard !Task.isCancelled else { return }
                    self.pairingProgress = .enrolled(name: self.pendingPhoneName ?? "Device")
                    await self.refresh()
                    self.restartSupervision()
                } catch {
                    guard !Task.isCancelled else { return }
                    self.pairingProgress = .failed(error.localizedDescription)
                    self.errorMessage = error.localizedDescription
                }
                self.enrollmentHelper = nil
            }
        } catch {
            pairingProgress = .idle
            pairingFailure = error.localizedDescription
            errorMessage = error.localizedDescription
        }
    }

    func dismissPairing() {
        enrollmentDecision?.resume(returning: false)
        enrollmentDecision = nil
        if let enrollmentHelper {
            enrollmentHelper.stop()
            Self.supervisorRegistry.remove(enrollmentHelper)
        }
        enrollmentHelper = nil
        if let enrollmentID = pendingPairing?.enrollmentID {
            Task { [controlPlane] in await controlPlane.cancelRemoteEnrollment(enrollmentID) }
        }
        enrollmentWatch?.cancel()
        enrollmentWatch = nil
        pendingPairing = nil
        pendingPhoneName = nil
        pairingProgress = .idle
    }

    private func awaitOwnerDecision(_ proposal: RemoteEnrollmentPending) async -> Bool {
        guard proposal.type == "enrollment_pending", proposal.version == 1,
              proposal.enrollmentId == pendingPairing?.enrollmentID else { return false }
        pendingPhoneName = proposal.name
        pairingProgress = .comparing(
            name: proposal.name, permission: proposal.permission, code: proposal.comparison
        )
        return await withCheckedContinuation { enrollmentDecision = $0 }
    }

    func approveEnrollment() {
        enrollmentDecision?.resume(returning: true)
        enrollmentDecision = nil
        pairingProgress = .waiting
    }

    func rejectEnrollment() {
        enrollmentDecision?.resume(returning: false)
        enrollmentDecision = nil
        enrollmentHelper?.stop()
        pairingProgress = .failed("Enrollment was rejected on this Mac.")
    }

    func grant(_ device: RemoteDevice, permission: DevicePermission) async {
        guard permission != device.permission else { return }
        do {
            try await client.grantRemoteDevice(device.deviceID, permission: permission)
            await refresh()
            if let directoryID = device.controlPlaneDeviceID {
                try await controlPlane.mirrorPermission(clientDeviceID: directoryID, permission: permission)
            }
            errorMessage = nil
            restartSupervision()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func setTerminalAllowed(_ allowed: Bool, for device: RemoteDevice) async {
        await grant(device, permission: allowed ? .control : device.permissionWithoutTerminal)
    }

    func revoke(_ device: RemoteDevice) async {
        do {
            // Local revocation is first and immediately terminates authorization.
            try await client.revokeRemoteDevice(device.deviceID)
            await refresh()
            if let directoryID = device.controlPlaneDeviceID {
                try await controlPlane.revokePairing(clientDeviceID: directoryID)
            }
            errorMessage = nil
            restartSupervision()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func installTerminationHandler() {
        guard terminationObserver == nil else { return }
        terminationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { _ in Self.terminateHelpers() }
    }

    private static var macName: String {
        Host.current().localizedName ?? ProcessInfo.processInfo.hostName
    }
}

final class SupervisorRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var supervisors: [any RemoteAccessProcess] = []

    func register(_ supervisor: any RemoteAccessProcess) {
        lock.lock(); defer { lock.unlock() }
        supervisors.append(supervisor)
    }

    func remove(_ supervisor: any RemoteAccessProcess) {
        lock.lock(); defer { lock.unlock() }
        supervisors.removeAll { ($0 as AnyObject) === (supervisor as AnyObject) }
    }

    func drain() -> [any RemoteAccessProcess] {
        lock.lock(); defer { lock.unlock() }
        let current = supervisors
        supervisors.removeAll()
        return current
    }
}
