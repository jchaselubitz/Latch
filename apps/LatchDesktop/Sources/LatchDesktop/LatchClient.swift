import Foundation
import Darwin

actor LatchClient {
    static let minimumProtocolVersion: UInt32 = 1
    static let preferences = UserDefaults(suiteName: "co.cooperativ.latch.desktop") ?? .standard

    private let executableURL: URL
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
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "latch") {
            return bundled
        }
        for path in ["/opt/homebrew/bin/latch", "/usr/local/bin/latch"]
        where FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }
        return URL(fileURLWithPath: "/usr/local/bin/latch")
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
    func remove(_ id: String, force: Bool) throws -> RemoveReport {
        try request(["remove", id] + (force ? ["--force"] : []) + ["--json"])
    }
    func previewPrune() throws -> PruneReport {
        try request(["prune", "--dry-run", "--all", "--json"])
    }
    func pruneAll() throws -> PruneReport { try request(["prune", "--all", "--json"]) }
    func doctor() throws -> DoctorReport { try request(["doctor", "--json"]) }

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

    private func request<Response: Decodable>(_ arguments: [String], stdin: Data? = nil) throws -> Response {
        let output = try ProcessRunner.run(
            executableURL: executableURL,
            arguments: arguments,
            stdin: stdin,
            timeout: timeout
        )
        do {
            return try decoder.decode(Response.self, from: output)
        } catch {
            throw LatchClientError.invalidResponse(error.localizedDescription)
        }
    }

    private static func loginShell() -> String {
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
        let stdout = Pipe()
        let stderr = Pipe()
        let finished = DispatchSemaphore(value: 0)
        process.executableURL = executableURL
        process.arguments = arguments
        process.standardOutput = stdout
        process.standardError = stderr
        process.terminationHandler = { _ in finished.signal() }

        if let stdin {
            let input = Pipe()
            process.standardInput = input
            try process.run()
            input.fileHandleForWriting.write(stdin)
            try? input.fileHandleForWriting.close()
        } else {
            try process.run()
        }
        if finished.wait(timeout: .now() + timeout) == .timedOut {
            process.terminate()
            throw LatchClientError.timeout
        }
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        let diagnostic = stderr.fileHandleForReading.readDataToEndOfFile()
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

enum LatchClientError: LocalizedError, Equatable {
    case executableNotFound(String)
    case timeout
    case commandFailed(status: Int32, diagnostic: String)
    case invalidResponse(String)
    case incompatibleProtocol(expected: UInt32, actual: UInt32, productVersion: String)

    var errorDescription: String? {
        switch self {
        case .executableNotFound(let path):
            return "Latch CLI was not found at \(path). Choose a compatible latch executable in Settings."
        case .timeout:
            return "Latch did not respond before the management timeout. No session worker was terminated."
        case .commandFailed(_, let diagnostic):
            return diagnostic.isEmpty ? "The Latch command failed." : diagnostic
        case .invalidResponse(let detail):
            return "Latch returned an incompatible response: \(detail)"
        case .incompatibleProtocol(let expected, let actual, let version):
            return "Latch \(version) uses protocol \(actual); this app requires protocol \(expected)."
        }
    }
}

private extension String {
    var nilIfBlank: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
