import Foundation

/// Why the app refused to start, or could not keep running, the helper.
enum RemoteAccessSupervisorError: LocalizedError, Equatable {
    case unsafeBind(String)
    case forbiddenArgument(String)
    case relayServerRefused(String)
    case helperMissing(URL)
    case readinessTimeout
    case exited(status: Int32, diagnostic: String)

    var errorDescription: String? {
        switch self {
        case .unsafeBind(let bind):
            return "Refusing to start remote access: \(bind) is not a usable listener address for the authenticated transport."
        case .forbiddenArgument(let argument):
            return "Refusing to start remote access: `\(argument)` would expose the plaintext gateway."
        case .relayServerRefused(let url):
            return "Refusing to start remote access: `\(url)` is a relay, and the helper gathers against STUN servers only."
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

/// Launches and babysits the authenticated remote-access helper.
///
/// The dedicated `latch-remote` helper is the only process the app
/// starts for remote access. The helper — not this app — supervises the
/// plaintext `latch serve` gateway on an ephemeral loopback port with a
/// per-launch bearer token it mints itself. Keeping that split means the
/// desktop never holds the gateway credential and has no code path that could
/// bind the gateway anywhere but loopback.
final class RemoteAccessSupervisor: @unchecked Sendable {
    /// An unspecified address with an ephemeral port. The helper picks the
    /// port; the app never pins one, so nothing is predictable to a scanner.
    static let defaultBind = "0.0.0.0:0"

    /// Arguments that would either point the helper at the plaintext gateway
    /// or let that gateway be published off-host. None of them are ever
    /// produced by this app, and a caller-supplied vector containing one is
    /// refused rather than filtered.
    static let forbiddenArguments: Set<String> = ["serve", "--allow-remote", "--token-file"]

    private let executableURL: URL
    private let latchExecutableURL: URL
    private let bind: String
    private let iceServers: [String]
    private let lock = NSLock()
    private var process: Process?
    private var stoppedIntentionally = false

    /// - Parameter iceServers: STUN URLs for the helper's agent to gather
    ///   server-reflexive candidates against. Empty gathers host candidates
    ///   only, which reaches a LAN or a tailnet and nothing beyond either.
    init(
        executableURL: URL,
        bind: String = RemoteAccessSupervisor.defaultBind,
        iceServers: [String] = []
    ) {
        latchExecutableURL = executableURL
        self.executableURL = executableURL
            .deletingLastPathComponent()
            .appendingPathComponent("latch-remote")
        self.bind = bind
        self.iceServers = iceServers
    }

    /// The argument vector used to launch the helper.
    ///
    /// Exposed for tests: the guarantee that the desktop app never publishes
    /// `latch serve` is only as good as what it actually execs.
    ///
    /// A relay URL is refused here, before the helper refuses it too. The
    /// helper's flag is STUN-only by contract, and the app must not be the
    /// place that contract quietly stops holding: a TURN allocation is made
    /// by the phone under credentials issued to the phone, never by this Mac.
    static func arguments(
        bind: String,
        latchExecutable: String = "/usr/local/bin/latch",
        iceServers: [String] = []
    ) throws -> [String] {
        try validate(bind: bind)
        var arguments = ["--bind", bind, "--latch-bin", latchExecutable]
        for server in iceServers {
            guard ControlPlaneIceServer.isStun(server) else {
                throw RemoteAccessSupervisorError.relayServerRefused(server)
            }
            arguments += ["--ice-server", server]
        }
        if let forbidden = arguments.first(where: { forbiddenArguments.contains($0) }) {
            throw RemoteAccessSupervisorError.forbiddenArgument(forbidden)
        }
        return arguments
    }

    /// A LAN listener must be reachable by a paired phone, so loopback is not a
    /// safer choice here — it is a broken one. A loopback bind is refused so
    /// the failure is visible instead of silently producing a listener that no
    /// device can ever reach, and so the helper's listener can never be
    /// confused with the loopback gateway it supervises.
    static func validate(bind: String) throws {
        let host = hostComponent(of: bind)
        guard !host.isEmpty, !isLoopback(host) else {
            throw RemoteAccessSupervisorError.unsafeBind(bind)
        }
    }

    /// Splits `host:port`, tolerating the bracketed IPv6 form.
    static func hostComponent(of bind: String) -> String {
        if bind.hasPrefix("["), let close = bind.firstIndex(of: "]") {
            return String(bind[bind.index(after: bind.startIndex)..<close])
        }
        guard let separator = bind.lastIndex(of: ":") else { return bind }
        return String(bind[bind.startIndex..<separator])
    }

    static func isLoopback(_ host: String) -> Bool {
        let normalized = host.lowercased()
        return normalized == "localhost"
            || normalized == "::1"
            || normalized == "0:0:0:0:0:0:0:1"
            || normalized.hasPrefix("127.")
    }

    var isRunning: Bool {
        lock.withLock { process?.isRunning == true }
    }

    /// Starts the helper and resolves when it exits. Throws immediately if the
    /// launch itself is unsafe or fails.
    func run() async throws {
        let arguments = try Self.arguments(
            bind: bind,
            latchExecutable: latchExecutableURL.path,
            iceServers: iceServers
        )
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            throw RemoteAccessSupervisorError.helperMissing(executableURL)
        }
        let process = Process()
        let diagnostics = Pipe()
        process.executableURL = executableURL
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
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
        } catch {
            lock.withLock { self.process = nil }
            throw error
        }

        let reader = diagnostics.fileHandleForReading
        let captured = Task.detached(priority: .utility) {
            reader.readDataToEndOfFile()
        }

        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.global(qos: .utility).async {
                exited.wait()
                continuation.resume()
            }
        }

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
