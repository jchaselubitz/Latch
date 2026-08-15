import Foundation
import Darwin

actor LatchClient {
    static let minimumProtocolVersion: UInt32 = 1
    static let minimumProductVersion = "0.2608132217.0"
    static let preferences = UserDefaults(suiteName: "co.cooperativ.latch.desktop") ?? .standard
    static let installCommand = "curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash"

    nonisolated let executableURL: URL
    private let timeout: TimeInterval
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()

    init(executableURL: URL = LatchClient.defaultExecutableURL(), timeout: TimeInterval = 10) {
        self.executableURL = executableURL
        self.timeout = timeout
    }

    static func defaultExecutableURL() -> URL {
        if let configured = preferences.string(forKey: "latchExecutablePath"),
           !configured.isEmpty {
            return URL(fileURLWithPath: configured)
        }
        if let discovered = try? discoverExecutablePaths(), let first = discovered.first {
            return URL(fileURLWithPath: first)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/bin/latch")
    }

    /// Ask the user's login shell to run `where latch`, preserving the order
    /// it reports when more than one independently installed CLI is present.
    static func discoverExecutablePaths(shell: String? = nil) throws -> [String] {
        let process = Process()
        let stdout = Pipe()
        process.executableURL = URL(fileURLWithPath: shell ?? loginShell())
        process.arguments = ["-lc", "where latch"]
        process.standardOutput = stdout
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()

        guard process.terminationStatus == 0 else { return [] }
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        return executablePaths(in: String(decoding: output, as: UTF8.self))
    }

    static func executablePaths(in whereOutput: String) -> [String] {
        var seen = Set<String>()
        return whereOutput.split(whereSeparator: \.isNewline).compactMap { line in
            let path = line.trimmingCharacters(in: .whitespacesAndNewlines)
            guard path.hasPrefix("/"),
                  FileManager.default.isExecutableFile(atPath: path),
                  seen.insert(path).inserted else { return nil }
            return path
        }
    }

    func validateCompatibility() throws -> CapabilitiesReport {
        let report: CapabilitiesReport = try request(["capabilities", "--json"])
        guard report.protocolVersion == Self.minimumProtocolVersion else {
            throw LatchClientError.incompatibleProtocol(
                expected: Self.minimumProtocolVersion,
                actual: report.protocolVersion,
                productVersion: report.productVersion
            )
        }
        guard let actual = ReleaseVersion(report.productVersion),
              let minimum = ReleaseVersion(Self.minimumProductVersion),
              actual >= minimum else {
            throw LatchClientError.incompatibleProduct(
                minimum: Self.minimumProductVersion,
                actual: report.productVersion
            )
        }
        return report
    }

    func list() throws -> ListReport { try request(["list", "--json"]) }
    func inspect(_ id: String) throws -> InspectReport { try request(["inspect", id, "--json"]) }
    func stop(_ id: String, force: Bool) throws -> StopReport {
        try request(["stop", id] + (force ? ["--force"] : []) + ["--json"])
    }
    func rename(_ id: String, to name: String) throws -> RenameReport {
        try request(["rename", id, name, "--json"])
    }
    func resize(_ id: String, request: ResizeSessionRequest) throws -> ResizeReport {
        try self.request(
            [
                "resize", id,
                "--cols", String(request.cols),
                "--rows", String(request.rows),
            ] + (request.pin ? ["--pin"] : []) + ["--json"]
        )
    }
    func remove(_ id: String, force: Bool) throws -> RemoveReport {
        try request(["remove", id] + (force ? ["--force"] : []) + ["--json"])
    }
    func previewPrune() throws -> PruneReport {
        try request(["prune", "--dry-run", "--all", "--json"])
    }
    func pruneAll() throws -> PruneReport { try request(["prune", "--all", "--json"]) }
    func doctor() throws -> DoctorReport { try request(["doctor", "--json"]) }
    func checkForUpdate() throws -> CLIUpdateReport {
        try request(["update", "--check", "--json"], timeout: 30)
    }
    func update() throws -> CLIUpdateReport {
        try request(["update", "--json"], timeout: 120)
    }

    func create(_ request: NewSessionRequest) throws -> CreateReport {
        let shell = Self.loginShell()
        let argv = request.command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? [shell, "-l"]
            : [shell, "-lc", request.command]
        let manifest = LaunchManifest(
            launch: .init(
                argv: argv,
                cwd: request.cwd,
                size: .init(cols: request.cols, rows: request.rows)
            ),
            display: .init(
                name: request.name.nilIfBlank,
                title: request.title.nilIfBlank,
                commandLabel: request.command.nilIfBlank.map { _ in shell }
            )
        )
        return try self.request(
            ["create", "--manifest-file", "-", "--json"],
            stdin: encoder.encode(manifest)
        )
    }

    func attachmentCommand(for id: String) -> [String] {
        [executableURL.path, "attach", id]
    }

    // MARK: - Remote access

    /// Reads the remote-access lifecycle without starting anything. Safe to
    /// poll while remote access is off: it never creates the Mac identity.
    func remoteAccessStatus() throws -> RemoteAccessStatus {
        try request(["remote-access", "status", "--json"])
    }

    /// Turns remote access on. The CLI creates and stores the Mac device
    /// identity on first enable (Keychain-backed private key on macOS).
    func enableRemoteAccess() throws {
        try run(["remote-access", "enable"])
    }

    /// The global off switch: refuses new connections, cancels pending pairing
    /// material, and removes the supervised gateway credential.
    func disableRemoteAccess() throws {
        try run(["remote-access", "disable"])
    }

    func remoteDevices() throws -> [RemoteDevice] {
        try request(["remote-access", "devices", "--json"])
    }

    func grantRemoteDevice(_ deviceID: String, permission: DevicePermission) throws {
        try run(["remote-access", "grant", deviceID, permission.rawValue])
    }

    func revokeRemoteDevice(_ deviceID: String) throws {
        try run(["remote-access", "revoke", deviceID])
    }

    func createRemotePairing() throws -> PairingMaterial {
        try request(["remote-access", "pair", "create", "--json"])
    }

    func setRemoteRelayEnabled(_ enabled: Bool) throws {
        try run(["remote-access", "relay", enabled ? "enable" : "disable"])
    }

    func remoteAudit() throws -> [RemoteAuditEvent] {
        try request(["remote-access", "audit", "--json"])
    }

    func remoteDiagnostics() throws -> RemoteDiagnostics {
        try request(["remote-access", "diagnostics"])
    }

    /// Runs a command whose success is the whole result. Output is discarded so
    /// a human-readable confirmation line never has to be parsed.
    private func run(_ arguments: [String], timeout: TimeInterval? = nil) throws {
        _ = try ProcessRunner.run(
            executableURL: executableURL,
            arguments: arguments,
            stdin: nil,
            timeout: timeout ?? self.timeout
        )
    }

    private func request<Response: Decodable>(
        _ arguments: [String],
        stdin: Data? = nil,
        timeout: TimeInterval? = nil
    ) throws -> Response {
        let output = try ProcessRunner.run(
            executableURL: executableURL,
            arguments: arguments,
            stdin: stdin,
            timeout: timeout ?? self.timeout
        )
        do {
            return try decoder.decode(Response.self, from: output)
        } catch {
            throw LatchClientError.invalidResponse(error.localizedDescription)
        }
    }

    static func loginShell() -> String {
        guard let record = getpwuid(getuid()), let value = record.pointee.pw_shell else {
            return "/bin/zsh"
        }
        return String(cString: value)
    }
}

private enum ProcessRunner {
    static func run(
        executableURL: URL,
        arguments: [String],
        stdin: Data?,
        timeout: TimeInterval
    ) throws -> Data {
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            throw LatchClientError.executableNotFound(executableURL.path)
        }

        let process = Process()
        let stdout = PipeCapture()
        let stderr = PipeCapture()
        let finished = DispatchSemaphore(value: 0)
        process.executableURL = executableURL
        process.arguments = arguments
        process.standardOutput = stdout.pipe
        process.standardError = stderr.pipe
        process.terminationHandler = { _ in finished.signal() }

        let input: Pipe?
        if stdin != nil {
            let pipe = Pipe()
            input = pipe
            process.standardInput = pipe
        } else {
            input = nil
        }
        try process.run()
        stdout.start()
        stderr.start()

        if let stdin {
            input?.fileHandleForWriting.write(stdin)
            try? input?.fileHandleForWriting.close()
        }
        if finished.wait(timeout: .now() + timeout) == .timedOut {
            process.terminate()
            var didExit = finished.wait(timeout: .now() + 2) == .success
            if !didExit {
                kill(process.processIdentifier, SIGKILL)
                didExit = finished.wait(timeout: .now() + 2) == .success
            }
            if didExit {
                _ = stdout.finish()
                _ = stderr.finish()
            } else {
                stdout.cancel()
                stderr.cancel()
            }
            throw LatchClientError.timeout
        }
        let output = stdout.finish()
        let diagnostic = stderr.finish()
        guard process.terminationStatus == 0 else {
            let bounded = diagnostic.suffix(8_192)
            throw LatchClientError.commandFailed(
                status: process.terminationStatus,
                diagnostic: String(decoding: bounded, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            )
        }
        return output
    }
}

/// Drains a child pipe while it runs so a response larger than the kernel pipe
/// buffer cannot deadlock the process before the timeout is observed.
private final class PipeCapture: @unchecked Sendable {
    let pipe = Pipe()

    private let group = DispatchGroup()
    private let lock = NSLock()
    private var data = Data()

    func start() {
        group.enter()
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            let captured = pipe.fileHandleForReading.readDataToEndOfFile()
            lock.lock()
            data = captured
            lock.unlock()
            group.leave()
        }
    }

    func finish() -> Data {
        group.wait()
        lock.lock()
        defer { lock.unlock() }
        return data
    }

    func cancel() {
        try? pipe.fileHandleForReading.close()
    }
}

enum LatchClientError: LocalizedError, Equatable {
    case executableNotFound(String)
    case timeout
    case commandFailed(status: Int32, diagnostic: String)
    case invalidResponse(String)
    case incompatibleProtocol(expected: UInt32, actual: UInt32, productVersion: String)
    case incompatibleProduct(minimum: String, actual: String)

    var errorDescription: String? {
        switch self {
        case .executableNotFound(let path):
            return "Latch CLI was not found at \(path). Choose an installed CLI in Settings or run the install command shown there."
        case .timeout:
            return "Latch did not respond before the management timeout. No session worker was terminated."
        case .commandFailed(_, let diagnostic):
            return diagnostic.isEmpty ? "The Latch command failed." : diagnostic
        case .invalidResponse(let detail):
            return "Latch returned an incompatible response: \(detail)"
        case .incompatibleProtocol(let expected, let actual, let version):
            return "Latch \(version) uses protocol \(actual); this app requires protocol \(expected)."
        case .incompatibleProduct(let minimum, let actual):
            return "Latch CLI \(actual) is too old for this app. Install or update to \(minimum) or newer."
        }
    }
}

private extension String {
    var nilIfBlank: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
