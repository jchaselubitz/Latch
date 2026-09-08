import Foundation

/// Why the app refused to start, or could not keep running, the helper.
enum RemoteAccessSupervisorError: LocalizedError, Equatable {
    case forbiddenArgument(String)
    case helperMissing(URL)
    case readinessTimeout
    case exited(status: Int32, diagnostic: String)

    var errorDescription: String? {
        switch self {
        case .forbiddenArgument(let argument):
            return "Refusing to start remote access: `\(argument)` would expose the plaintext gateway."
        case .helperMissing(let url):
            return "The remote-access helper is missing or not executable at \(url.path). It is installed next to the Latch CLI; run `latch update` or the installer in Settings → Latch CLI to repair the complete payload."
        case .readinessTimeout:
            return "Remote access started but never advertised a listener. Nothing was exposed."
        case .exited(let status, let diagnostic):
            return diagnostic.isEmpty
                ? "The remote access helper exited with status \(status)."
                : diagnostic
        }
    }
}

/// The only secret-bearing launch input. Encoding is written to the helper's
/// inherited stdin and never placed in argv, environment, defaults, or disk.
struct RemoteLinkHostConfiguration: Encodable, Equatable, Sendable {
    let version: Int
    let purpose: String
    let relayUrl: String
    let admission: String
    let peerPublicKey: String?
    let grantRevision: UInt64
    let enrollmentId: String?
    let enrollmentSecret: String?

    init(
        version: Int = 1,
        purpose: String = "session",
        relayUrl: String,
        admission: String,
        peerPublicKey: String? = nil,
        grantRevision: UInt64 = 0,
        enrollmentId: String? = nil,
        enrollmentSecret: String? = nil
    ) {
        self.version = version
        self.purpose = purpose
        self.relayUrl = relayUrl
        self.admission = admission
        self.peerPublicKey = peerPublicKey
        self.grantRevision = grantRevision
        self.enrollmentId = enrollmentId
        self.enrollmentSecret = enrollmentSecret
    }
}

/// Launches and babysits the authenticated remote-access helper.
///
/// The dedicated `latch-remote` helper is the only process the app
/// starts for remote access. The helper — not this app — supervises the
/// plaintext `latch serve` gateway on an ephemeral loopback port with a
/// per-launch bearer token it mints itself. Keeping that split means the
/// desktop never holds the gateway credential and has no code path that could
/// bind the gateway anywhere but loopback.
final class RemoteAccessSupervisor: @unchecked Sendable {
    /// Arguments that would either point the helper at the plaintext gateway
    /// or let that gateway be published off-host. None of them are ever
    /// produced by this app, and a caller-supplied vector containing one is
    /// refused rather than filtered.
    static let forbiddenArguments: Set<String> = ["serve", "--allow-remote", "--token-file"]

    typealias LeaseRenewal = @Sendable (String) async throws -> ControlPlaneLeaseExtension
    typealias AdmissionRequest = @Sendable () async throws -> RemoteLinkAdmission
    typealias StatusHandler = @Sendable (HelperLinkStatus) -> Void

    /// One line the helper printed, classified. Anything the app does not
    /// understand is `ignored` rather than fatal: the helper owns the link,
    /// and a status it invents later must not take supervision down.
    enum HelperEvent: Equatable, Sendable {
        case status(HelperLinkStatus)
        case leaseStarted(leaseID: String, expiresAt: UInt64)
        case admissionNeeded(reason: String)
        case ignored
    }

    private let executableURL: URL
    private let latchExecutableURL: URL
    private var configuration: RemoteLinkHostConfiguration?
    private let renewLease: LeaseRenewal
    private let requestAdmission: AdmissionRequest?
    private let onStatus: StatusHandler?
    private let lock = NSLock()
    private var process: Process?
    private var stoppedIntentionally = false

    init(
        executableURL: URL,
        configuration: RemoteLinkHostConfiguration,
        renewLease: @escaping LeaseRenewal,
        requestAdmission: AdmissionRequest? = nil,
        onStatus: StatusHandler? = nil
    ) {
        latchExecutableURL = executableURL
        self.executableURL = executableURL
            .deletingLastPathComponent()
            .appendingPathComponent("latch-remote")
        self.configuration = configuration
        self.renewLease = renewLease
        self.requestAdmission = requestAdmission
        self.onStatus = onStatus
    }

    /// Classifies one helper stdout line. Exposed for tests.
    static func parseEvent(_ line: String) -> HelperEvent? {
        struct Envelope: Decodable {
            let type: String
            let version: Int
            let status: String?
            let leaseId: String?
            let expiresAt: UInt64?
            let reason: String?
        }
        guard let data = line.data(using: .utf8),
              let event = try? JSONDecoder().decode(Envelope.self, from: data),
              event.version == 1
        else { return nil }
        switch event.type {
        case "status":
            guard let raw = event.status, let status = HelperLinkStatus(rawValue: raw) else { return .ignored }
            return .status(status)
        case "lease_started":
            guard let leaseID = event.leaseId, let expiresAt = event.expiresAt else { return nil }
            return .leaseStarted(leaseID: leaseID, expiresAt: expiresAt)
        case "admission_needed":
            return .admissionNeeded(reason: event.reason ?? "")
        default:
            return .ignored
        }
    }

    /// The argument vector used to launch the helper.
    ///
    /// Exposed for tests: the guarantee that the desktop app never publishes
    /// `latch serve` is only as good as what it actually execs.
    ///
    static func arguments(
        latchExecutable: String = "/usr/local/bin/latch"
    ) throws -> [String] {
        let arguments = ["--link-serve", "--latch-bin", latchExecutable]
        if let forbidden = arguments.first(where: { forbiddenArguments.contains($0) }) {
            throw RemoteAccessSupervisorError.forbiddenArgument(forbidden)
        }
        return arguments
    }

    var isRunning: Bool {
        lock.withLock { process?.isRunning == true }
    }

    /// Starts the helper and resolves when it exits. Throws immediately if the
    /// launch itself is unsafe or fails.
    func run() async throws {
        let arguments = try Self.arguments(latchExecutable: latchExecutableURL.path)
        let encoded = try lock.withLock { () throws -> Data in
            guard let configuration else {
                throw RemoteAccessSupervisorError.exited(status: -1, diagnostic: "Remote Link admission was already consumed.")
            }
            self.configuration = nil
            return try JSONEncoder().encode(configuration)
        }
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            throw RemoteAccessSupervisorError.helperMissing(executableURL)
        }
        let process = Process()
        let diagnostics = Pipe()
        let input = Pipe()
        let events = Pipe()
        process.executableURL = executableURL
        process.arguments = arguments
        process.standardInput = input
        process.standardOutput = events
        process.standardError = diagnostics

        // The handler is installed before launch: a helper that dies during
        // startup must still resolve this call rather than hang supervision.
        let exited = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in exited.signal() }

        lock.withLock {
            stoppedIntentionally = false
            self.process = process
        }

        do {
            try process.run()
            var initial = encoded
            initial.append(0x0a)
            try input.fileHandleForWriting.write(contentsOf: initial)
        } catch {
            try? input.fileHandleForWriting.close()
            lock.withLock { self.process = nil }
            throw error
        }

        let reader = diagnostics.fileHandleForReading
        let captured = Task.detached(priority: .utility) {
            reader.readDataToEndOfFile()
        }
        let writer = HelperCommandWriter(handle: input.fileHandleForWriting)
        let renewal = Task.detached(priority: .utility) { [renewLease, requestAdmission, onStatus] in
            var lease: Task<Void, Never>?
            defer { lease?.cancel() }
            for try await line in events.fileHandleForReading.bytes.lines {
                switch Self.parseEvent(line) {
                case .status(let status)?:
                    onStatus?(status)
                case .leaseStarted(let leaseID, let expiresAt)?:
                    // Every relay socket brings its own lease; the renewal
                    // clock follows the newest one and drops the old.
                    lease?.cancel()
                    lease = Task.detached(priority: .utility) {
                        await Self.renew(leaseID: leaseID, expiresAt: expiresAt, renewLease: renewLease, writer: writer)
                    }
                case .admissionNeeded?:
                    lease?.cancel()
                    lease = nil
                    guard let requestAdmission else { continue }
                    await Self.readmit(requestAdmission: requestAdmission, writer: writer)
                case .ignored?, nil:
                    continue
                }
            }
        }

        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.global(qos: .utility).async {
                exited.wait()
                continuation.resume()
            }
        }
        renewal.cancel()
        try? input.fileHandleForWriting.close()
        try? events.fileHandleForReading.close()

        let intentional = lock.withLock {
            let intentional = stoppedIntentionally
            self.process = nil
            return intentional
        }

        guard !intentional else { return }
        let bounded = await captured.value.suffix(4_096)
        throw RemoteAccessSupervisorError.exited(
            status: process.terminationStatus,
            diagnostic: String(decoding: bounded, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }

    /// Renews one lease at the five-minute mark until the task is cancelled
    /// or the control plane refuses; the relay's own deadline then closes the
    /// socket, which is the fail-closed outcome.
    private static func renew(
        leaseID: String,
        expiresAt: UInt64,
        renewLease: LeaseRenewal,
        writer: HelperCommandWriter
    ) async {
        var expiry = expiresAt
        while !Task.isCancelled {
            let now = UInt64(Date().timeIntervalSince1970)
            let delay = expiry > now + 300 ? expiry - now - 300 : 0
            if delay > 0 {
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                if Task.isCancelled { return }
            }
            guard let extensionValue = try? await renewLease(leaseID), extensionValue.leaseId == leaseID else {
                return
            }
            writer.write(HelperCommand.leaseExtension(leaseID: leaseID, claim: extensionValue.claim))
            expiry = extensionValue.expiresAt
        }
    }

    /// Fetches a fresh single-use admission for the helper's next relay socket,
    /// with bounded backoff while the control plane is unreachable. The helper
    /// keeps its gateway and LAN listener alive throughout.
    private static func readmit(requestAdmission: AdmissionRequest, writer: HelperCommandWriter) async {
        let delays: [UInt64] = [1, 2, 5, 10, 15]
        var attempt = 0
        while !Task.isCancelled {
            if let admission = try? await requestAdmission() {
                writer.write(HelperCommand.admission(relayURL: admission.relayURL, admission: admission.admission))
                return
            }
            let delay = delays[min(attempt, delays.count - 1)]
            attempt += 1
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
        }
    }

    /// Stops the helper. The helper takes the supervised gateway down with it,
    /// because it spawned that child with kill-on-drop.
    func stop() {
        let running = lock.withLock { () -> Process? in
            stoppedIntentionally = true
            return process
        }
        guard let running, running.isRunning else { return }
        running.terminate()
    }
}

/// Lines written to the helper's stdin after the initial configuration.
enum HelperCommand {
    static func leaseExtension(leaseID: String, claim: String) -> Data {
        encode(["type": "lease_extension", "version": 1, "leaseId": leaseID, "claim": claim])
    }

    static func admission(relayURL: String, admission: String) -> Data {
        encode(["type": "admission", "version": 1, "relayUrl": relayURL, "admission": admission])
    }

    private static func encode(_ object: [String: Any]) -> Data {
        var data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data()
        data.append(0x0a)
        return data
    }
}

/// Serializes writes to the helper's stdin from the renewal and re-admission
/// tasks, which run concurrently.
final class HelperCommandWriter: @unchecked Sendable {
    private let handle: FileHandle
    private let lock = NSLock()

    init(handle: FileHandle) { self.handle = handle }

    func write(_ line: Data) {
        lock.withLock { try? handle.write(contentsOf: line) }
    }
}

/// Owns one provisional enrollment helper from QR display through encrypted
/// receipt. The helper itself commits the Mac-local grant; Desktop can only
/// answer the visible owner decision and attest that the service mirror
/// returned the identical grant.
final class RemoteEnrollmentSupervisor: @unchecked Sendable {
    private struct Envelope: Decodable { let type: String; let version: Int }
    private struct Committed: Decodable {
        let type: String
        let version: Int
        let enrollmentId: String
        let provisionalDeviceId: String
        let controllerPublicKey: String
        let permission: DevicePermission
        let grantRevision: UInt64
    }
    private struct Decision: Encodable {
        let type = "enrollment_decision"
        let version = 1
        let enrollmentId: String
        let provisionalDeviceId: String
        let controllerPublicKey: String
        let permission: DevicePermission
        let approved: Bool
    }
    private struct Mirrored: Encodable {
        let type = "enrollment_mirrored"
        let version = 1
        let enrollmentId: String
        let provisionalDeviceId: String
        let controllerPublicKey: String
        let permission: DevicePermission
        let grantRevision: UInt64
    }

    private let executableURL: URL
    private let latchExecutableURL: URL
    private let configuration: RemoteLinkHostConfiguration
    private let lock = NSLock()
    private var process: Process?

    init(executableURL: URL, configuration: RemoteLinkHostConfiguration) {
        latchExecutableURL = executableURL
        self.executableURL = executableURL
            .deletingLastPathComponent()
            .appendingPathComponent("latch-remote")
        self.configuration = configuration
    }

    func run(
        decide: @escaping @Sendable (RemoteEnrollmentPending) async -> Bool,
        mirror: @escaping @Sendable (
            String, String, DevicePermission, UInt64
        ) async throws -> ControlPlaneEnrollmentReceipt
    ) async throws {
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            throw RemoteAccessSupervisorError.helperMissing(executableURL)
        }
        let process = Process()
        let input = Pipe()
        let output = Pipe()
        let diagnostics = Pipe()
        process.executableURL = executableURL
        process.arguments = try RemoteAccessSupervisor.arguments(
            latchExecutable: latchExecutableURL.path
        )
        process.standardInput = input
        process.standardOutput = output
        process.standardError = diagnostics
        lock.withLock { self.process = process }
        do {
            try process.run()
            try write(configuration, to: input.fileHandleForWriting)
            for try await line in output.fileHandleForReading.bytes.lines {
                guard let data = line.data(using: .utf8) else { continue }
                let envelope = try JSONDecoder().decode(Envelope.self, from: data)
                guard envelope.version == 1 else {
                    throw RemoteAccessSupervisorError.exited(
                        status: -1,
                        diagnostic: "Enrollment helper emitted an unsupported protocol version."
                    )
                }
                switch envelope.type {
                case "enrollment_pending":
                    let pending = try JSONDecoder().decode(RemoteEnrollmentPending.self, from: data)
                    try write(Decision(
                        enrollmentId: pending.enrollmentId,
                        provisionalDeviceId: pending.provisionalDeviceId,
                        controllerPublicKey: pending.controllerPublicKey,
                        permission: pending.permission,
                        approved: await decide(pending)
                    ), to: input.fileHandleForWriting)
                case "enrollment_committed":
                    let committed = try JSONDecoder().decode(Committed.self, from: data)
                    let receipt = try await mirror(
                        committed.enrollmentId,
                        committed.controllerPublicKey,
                        committed.permission,
                        committed.grantRevision
                    )
                    guard receipt.version == 1,
                          receipt.enrollmentId == committed.enrollmentId,
                          receipt.controllerPublicKey == committed.controllerPublicKey,
                          receipt.permission == committed.permission,
                          receipt.grantRevision == committed.grantRevision
                    else {
                        throw RemoteAccessSupervisorError.exited(
                            status: -1,
                            diagnostic: "Control-plane enrollment mirror did not match the local grant."
                        )
                    }
                    try write(Mirrored(
                        enrollmentId: committed.enrollmentId,
                        provisionalDeviceId: committed.provisionalDeviceId,
                        controllerPublicKey: committed.controllerPublicKey,
                        permission: committed.permission,
                        grantRevision: committed.grantRevision
                    ), to: input.fileHandleForWriting)
                default:
                    continue
                }
            }
            process.waitUntilExit()
            if process.terminationStatus != 0 {
                let data = diagnostics.fileHandleForReading.readDataToEndOfFile().suffix(4_096)
                throw RemoteAccessSupervisorError.exited(
                    status: process.terminationStatus,
                    diagnostic: String(decoding: data, as: UTF8.self)
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                )
            }
        } catch {
            if process.isRunning { process.terminate() }
            throw error
        }
        lock.withLock { self.process = nil }
    }

    func stop() {
        let running = lock.withLock { process }
        if running?.isRunning == true { running?.terminate() }
    }

    private func write(_ value: some Encodable, to handle: FileHandle) throws {
        var data = try JSONEncoder().encode(value)
        data.append(0x0a)
        try handle.write(contentsOf: data)
    }
}
