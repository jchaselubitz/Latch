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
    private var pollingTask: Task<Void, Never>?
    private var enrollmentWatch: Task<Void, Never>?
    private var terminationObserver: NSObjectProtocol?

    init(client: LatchClient = LatchClient(), controlPlane: ControlPlaneHost = ControlPlaneHost()) {
        self.client = client
        self.controlPlane = controlPlane
        self.controlPlaneAddress = controlPlane.address?.absoluteString ?? ""
    }

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
    func restoreIfEnabled() async {
        installTerminationHandler()
        await refresh()
        if status.enabled, supervision == nil {
            startSupervision()
        }
        startPolling()
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
            phase = .failed(error.localizedDescription)
            errorMessage = error.localizedDescription
            await refresh()
        }
    }

    private func startSupervision() {
        guard supervision == nil else { return }
        phase = .starting
        let executableURL = client.executableURL
        supervision = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
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
                    await self?.recordHelperFailure(error)
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
        // Cancelling the task is not enough on its own: the child is only
        // reaped once it is asked to terminate.
        RemoteAccessController.terminateHelpers()
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

    private func startPolling() {
        guard pollingTask == nil else { return }
        pollingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                guard !Task.isCancelled else { return }
                await self?.refresh()
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
            status = try await client.remoteAccessStatus()
            devices = try await client.remoteDevices()
            auditEvents = (try? await client.remoteAudit()) ?? auditEvents
            if !status.enabled {
                phase = .off
            } else if let listener = status.listenerAddress {
                phase = .online(listener: listener)
            } else if case .failed = phase {
                // Keep the failure visible rather than downgrading it.
            } else if supervision != nil {
                phase = .starting
            }
        } catch {
            errorMessage = error.localizedDescription
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
        guard status.enabled else { return }
        enrollmentWatch?.cancel()
        enrollmentWatch = nil
        do {
            let material = try await client.createRemotePairing()
            guard controlPlane.isConfigured else {
                // A Mac with no control plane still pairs, but the phone has
                // to be told the address by hand.
                pendingPairing = material
                pairingProgress = .unaddressed
                errorMessage = nil
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
            await refresh()
            watchForEnrollment(addressed, known: known)
        } catch {
            pairingProgress = .idle
            errorMessage = error.localizedDescription
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
                permission: phone.permission ?? .interact
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
            try await client.grantRemoteDevice(device.deviceID, permission: permission)
            errorMessage = nil
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
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

    func setRelayEnabled(_ enabled: Bool) async {
        do {
            try await client.setRemoteRelayEnabled(enabled)
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
