import Foundation

/// Content-free stage durations for one link establishment, mirrored from the
/// shared Rust core. Milliseconds; zero when a stage did not happen.
public struct RemoteLinkStageTimings: Equatable, Sendable, Codable {
    /// Control-plane admission request (ticket minting), phone side.
    public var admissionMs: UInt64
    /// TCP/TLS/WebSocket connect, or LAN TCP connect.
    public var connectMs: UInt64
    /// Waiting for the relay to report the Mac present.
    public var peerWaitMs: UInt64
    /// Noise XX plus LinkHello.
    public var authenticateMs: UInt64

    public init(admissionMs: UInt64 = 0, connectMs: UInt64 = 0, peerWaitMs: UInt64 = 0, authenticateMs: UInt64 = 0) {
        self.admissionMs = admissionMs
        self.connectMs = connectMs
        self.peerWaitMs = peerWaitMs
        self.authenticateMs = authenticateMs
    }

    /// Tap-to-link-ready, before any gateway request.
    public var linkReadyMs: UInt64 { admissionMs + connectMs + peerWaitMs + authenticateMs }
}

/// Why a connection attempt failed, classified so the coordinator can decide
/// between retrying and stopping. Automatic retry must stop for anything a
/// retry cannot change.
public enum RemoteLinkFailure: Error, Equatable, Sendable {
    /// The relay admitted this phone but the Mac never joined within the wait.
    /// That is a statement about the relay room, not about the Mac's own
    /// internet connection, and the wording says only what was observed.
    case macOffline
    /// Pin, purpose, or protocol validation failed. Re-pairing is required.
    case authentication(String)
    /// The control plane no longer recognises this device or its pairing.
    case revoked(String)
    /// A deadline passed or the network failed; retrying is reasonable.
    case transient(String)

    public var message: String {
        switch self {
        case .macOffline:
            return "This phone could not find your Mac on the relay. It may be asleep, offline, or have remote access turned off."
        case .authentication(let detail):
            return "Your Mac refused this phone's identity: \(detail) Pair again from a new code."
        case .revoked(let detail):
            return detail
        case .transient(let detail):
            return detail
        }
    }

    /// Whether the owner may try again on its own.
    public var isRetryable: Bool {
        switch self {
        case .macOffline, .transient: return true
        case .authentication, .revoked: return false
        }
    }
}

/// One authenticated, multiplexed link the coordinator owns.
public protocol RemoteLinkConnection: AnyObject, Sendable {
    var path: RemotePath { get }
    var grantRevision: UInt64 { get }
    var timings: RemoteLinkStageTimings { get }
    func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel
    /// Resolves once the link has stopped for any reason.
    func waitClosed() async
    func close() async
}

/// How the coordinator should reach the Mac on this attempt.
public struct RemoteLinkConnectOptions: Equatable, Sendable {
    /// Diagnostics-only: skip the LAN attempt so the relay path is measured
    /// from a network where the Mac is also visible locally. Not a transport
    /// selector: both entry points speak the same protocol.
    public var skipLAN: Bool
    /// How long to wait for the relay to report the Mac present.
    public var peerWait: Duration

    public init(skipLAN: Bool = false, peerWait: Duration = .seconds(12)) {
        self.skipLAN = skipLAN
        self.peerWait = peerWait
    }
}

/// Establishes one link. Implemented by the native module over the shared
/// Rust core, and by fakes in tests.
public protocol RemoteLinkConnecting: Sendable {
    func connect(record: PairedDeviceRecord, options: RemoteLinkConnectOptions) async throws -> any RemoteLinkConnection
}

/// Where the one link is, as an immutable snapshot for UI.
public enum RemoteLinkState: Equatable, Sendable {
    /// No pairing, or the owner was stopped.
    case disabled
    /// Admission, transport connect, and authentication in progress.
    case connecting(attempt: Int)
    /// Authenticated and serving streams.
    case ready
    /// Waiting to retry a retryable failure.
    case backoff(attempt: Int, nextRetryAt: Date, reason: String)
    /// The relay admitted this phone but the Mac is not there. Retrying
    /// continues on the backoff schedule; the label claims only that the Mac
    /// was unavailable through the relay, never that it is offline.
    case macOffline(nextRetryAt: Date)
    /// The app is in the background; nothing is held.
    case suspended
    /// The control plane no longer recognises this device or pairing.
    case revoked(String)
    /// Authentication against the pinned Mac failed. Re-pair.
    case pairingRequired(String)

    public var isReady: Bool { self == .ready }

    public var isTerminal: Bool {
        switch self {
        case .revoked, .pairingRequired: return true
        default: return false
        }
    }
}

public struct RemoteLinkSnapshot: Equatable, Sendable {
    public var state: RemoteLinkState
    /// Increments for every authenticated link. Discovery runs once per value.
    public var generation: Int
    public var path: RemotePath?
    public var timings: RemoteLinkStageTimings?
    /// Set once a link is authenticated and cleared when it is lost.
    public var connectedAt: Date?

    public init(
        state: RemoteLinkState = .disabled,
        generation: Int = 0,
        path: RemotePath? = nil,
        timings: RemoteLinkStageTimings? = nil,
        connectedAt: Date? = nil
    ) {
        self.state = state
        self.generation = generation
        self.path = path
        self.timings = timings
        self.connectedAt = connectedAt
    }
}

/// Full-jitter exponential backoff, from 250 ms to 15 s.
public enum RemoteLinkBackoff {
    public static let floor: Duration = .milliseconds(250)
    public static let ceiling: Duration = .seconds(15)
    /// A link healthy this long resets the attempt count.
    public static let healthyReset: Duration = .seconds(30)

    /// The upper bound for `attempt` (zero-based). Jitter picks uniformly
    /// below it, so a burst of phones never reconnects in lockstep.
    public static func ceiling(forAttempt attempt: Int) -> Duration {
        let exponent = min(max(attempt, 0), 6)
        let base = Double(Self.floor.components.attoseconds) / 1e18
            + Double(Self.floor.components.seconds)
        let seconds = min(base * pow(2.0, Double(exponent)), 15.0)
        return .seconds(seconds)
    }

    public static func delay(forAttempt attempt: Int, random: Double = Double.random(in: 0...1)) -> Duration {
        let upper = ceiling(forAttempt: attempt)
        let seconds = Double(upper.components.seconds) + Double(upper.components.attoseconds) / 1e18
        return .seconds(max(0, seconds * min(max(random, 0), 1)))
    }
}

/// The one owner of the paired link.
///
/// Screens never open relay connections. They ask this actor for a gateway
/// channel and read its snapshot. It alone decides when to connect, when to
/// wait, and when to stop trying; the same owner absorbs foreground and
/// network-change triggers so there is never a second retry loop behind the
/// first.
public actor RemoteLinkCoordinator {
    /// A stream that never opens because the link is not usable and will not
    /// become usable inside the caller's patience.
    public enum ChannelError: Error, Equatable, Sendable {
        case notRunning
        case unavailable(RemoteLinkState)
        case timedOut
    }

    /// How long a channel request waits for a link that is connecting.
    public static let channelWait: Duration = .seconds(12)
    /// How long a health probe may take before the link is declared dead.
    public static let probeDeadline: Duration = .seconds(3)

    private let connector: any RemoteLinkConnecting
    private let clock: @Sendable () -> Date
    private let sleep: @Sendable (Duration) async throws -> Void
    private let random: @Sendable () -> Double

    private var record: PairedDeviceRecord?
    private var options = RemoteLinkConnectOptions()
    private var snapshot = RemoteLinkSnapshot()
    private var connection: (any RemoteLinkConnection)?
    private var supervisor: Task<Void, Never>?
    private var retryNow: CheckedContinuation<Void, Never>?
    private var retryTimer: Task<Void, Never>?
    private var readyWaiters: [UUID: CheckedContinuation<Void, Never>] = [:]
    private var observers: [UUID: AsyncStream<RemoteLinkSnapshot>.Continuation] = [:]
    private var attempts = 0
    private var probe: (@Sendable () async -> Bool)?

    public init(
        connector: any RemoteLinkConnecting,
        clock: @escaping @Sendable () -> Date = { Date() },
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
        random: @escaping @Sendable () -> Double = { Double.random(in: 0...1) }
    ) {
        self.connector = connector
        self.clock = clock
        self.sleep = sleep
        self.random = random
    }

    public var current: RemoteLinkSnapshot { snapshot }

    /// Snapshots, starting with the current one.
    public func snapshots() -> AsyncStream<RemoteLinkSnapshot> {
        let id = UUID()
        return AsyncStream { continuation in
            observers[id] = continuation
            continuation.yield(snapshot)
            continuation.onTermination = { [weak self] _ in
                Task { await self?.removeObserver(id) }
            }
        }
    }

    private func removeObserver(_ id: UUID) {
        observers.removeValue(forKey: id)
    }

    /// The application-level check `retryNow` runs on a link that looks
    /// alive: a bounded gateway request whose failure means the link is dead.
    public func setProbe(_ probe: (@Sendable () async -> Bool)?) {
        self.probe = probe
    }

    public func setOptions(_ options: RemoteLinkConnectOptions) {
        self.options = options
    }

    /// Begins owning the link for `record`. Restarting with a new record
    /// closes the previous link.
    public func start(record: PairedDeviceRecord) async {
        await stopSupervisor()
        self.record = record
        attempts = 0
        publish(RemoteLinkSnapshot(state: .connecting(attempt: 0), generation: snapshot.generation))
        supervisor = Task { [weak self] in await self?.supervise() }
    }

    /// Releases the link and everything behind it before the app suspends.
    public func suspend() async {
        await stopSupervisor()
        publish(RemoteLinkSnapshot(state: .suspended, generation: snapshot.generation))
    }

    /// Foreground: one immediate attempt on the same owner.
    public func resume() async {
        LinkTrace.shared.mark("coordinator.resume")
        guard let record else { return }
        if supervisor == nil {
            attempts = 0
            publish(RemoteLinkSnapshot(state: .connecting(attempt: 0), generation: snapshot.generation))
            supervisor = Task { [weak self] in await self?.supervise() }
        } else {
            await retryImmediately()
        }
    }

    /// Stops owning the link entirely.
    public func stop() async {
        await stopSupervisor()
        record = nil
        publish(RemoteLinkSnapshot(state: .disabled, generation: snapshot.generation))
    }

    /// A network path change or foreground event: cut any backoff short, or
    /// probe a link that looks alive and replace it if the probe fails.
    public func retryImmediately() async {
        switch snapshot.state {
        case .backoff, .macOffline:
            fireRetry()
        case .ready:
            guard let probe, let connection else { return }
            // The deadline uses the real clock: a probe is a network request,
            // and the point is to bound it in wall time.
            let healthy = await withTaskGroup(of: Bool.self) { group in
                group.addTask { await probe() }
                group.addTask {
                    try? await Task.sleep(for: Self.probeDeadline)
                    return false
                }
                let first = await group.next() ?? false
                group.cancelAll()
                return first
            }
            if !healthy, self.connection === connection {
                await connection.close()
            }
        case .connecting, .suspended, .disabled, .revoked, .pairingRequired:
            break
        }
    }

    /// Opens one gateway stream on the current link, waiting briefly for a
    /// link that is still connecting. Never opens a connection of its own.
    public func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
        if record == nil || supervisor == nil { throw ChannelError.notRunning }
        if snapshot.state.isTerminal { throw ChannelError.unavailable(snapshot.state) }
        if connection == nil {
            await waitUntilReady(limit: Self.channelWait)
            guard connection != nil else {
                if snapshot.state.isTerminal { throw ChannelError.unavailable(snapshot.state) }
                throw ChannelError.timedOut
            }
        }
        guard let connection else { throw ChannelError.timedOut }
        return try await connection.openGatewayChannel()
    }

    /// Parks the caller until a link is ready, the owner stops, or `limit`
    /// passes. Every path resumes the continuation exactly once.
    private func waitUntilReady(limit: Duration) async {
        if connection != nil { return }
        let id = UUID()
        await withCheckedContinuation { continuation in
            readyWaiters[id] = continuation
            Task { [weak self, sleep] in
                try? await sleep(limit)
                await self?.expireWaiter(id)
            }
        }
    }

    private func expireWaiter(_ id: UUID) {
        readyWaiters.removeValue(forKey: id)?.resume()
    }

    private func resumeReadyWaiters() {
        let waiters = readyWaiters
        readyWaiters = [:]
        waiters.values.forEach { $0.resume() }
    }

    /// Ends the current backoff wait, whatever started it.
    private func fireRetry() {
        retryTimer?.cancel()
        retryTimer = nil
        retryNow?.resume()
        retryNow = nil
    }

    // MARK: - Supervision

    private func supervise() async {
        while !Task.isCancelled, let record {
            publish(RemoteLinkSnapshot(state: .connecting(attempt: attempts), generation: snapshot.generation))
            let outcome: Result<any RemoteLinkConnection, Error>
            LinkTrace.shared.mark("coordinator.connect.begin")
            do {
                outcome = .success(try await connector.connect(record: record, options: options))
            } catch {
                outcome = .failure(error)
            }
            LinkTrace.shared.mark("coordinator.connect.end")
            if Task.isCancelled { return }
            switch outcome {
            case .success(let link):
                connection = link
                let connectedAt = clock()
                publish(RemoteLinkSnapshot(
                    state: .ready,
                    generation: snapshot.generation + 1,
                    path: link.path,
                    timings: link.timings,
                    connectedAt: connectedAt
                ))
                resumeReadyWaiters()
                await link.waitClosed()
                if Task.isCancelled { return }
                connection = nil
                // A link that lived a while earns a fresh schedule; one that
                // died immediately keeps climbing the backoff.
                if clock().timeIntervalSince(connectedAt) >= Double(RemoteLinkBackoff.healthyReset.components.seconds) {
                    attempts = 0
                }
                await backOff(reason: "The secure connection to your Mac was lost.", macOffline: false)
            case .failure(let error):
                let failure = Self.classify(error)
                switch failure {
                case .authentication(let detail):
                    publish(RemoteLinkSnapshot(state: .pairingRequired(detail), generation: snapshot.generation))
                    supervisor = nil
                    return
                case .revoked(let detail):
                    publish(RemoteLinkSnapshot(state: .revoked(detail), generation: snapshot.generation))
                    supervisor = nil
                    return
                case .macOffline:
                    await backOff(reason: failure.message, macOffline: true)
                case .transient(let detail):
                    await backOff(reason: detail, macOffline: false)
                }
            }
        }
    }

    private func backOff(reason: String, macOffline: Bool) async {
        let delay = RemoteLinkBackoff.delay(forAttempt: attempts, random: random())
        attempts += 1
        let nextRetryAt = clock().addingTimeInterval(
            Double(delay.components.seconds) + Double(delay.components.attoseconds) / 1e18
        )
        publish(RemoteLinkSnapshot(
            state: macOffline ? .macOffline(nextRetryAt: nextRetryAt) : .backoff(attempt: attempts, nextRetryAt: nextRetryAt, reason: reason),
            generation: snapshot.generation
        ))
        // Either the timer fires or a foreground/network event cuts it short;
        // both go through `fireRetry`, which resumes the one continuation once.
        retryTimer?.cancel()
        retryTimer = Task { [weak self, sleep] in
            try? await sleep(delay)
            await self?.fireRetry()
        }
        await withCheckedContinuation { continuation in
            retryNow = continuation
        }
    }

    static func traceWord(_ state: RemoteLinkState) -> String {
        switch state {
        case .disabled: return "disabled"
        case .connecting(let attempt): return "connecting.\(attempt)"
        case .ready: return "ready"
        case .backoff(let attempt, _, _): return "backoff.\(attempt)"
        case .macOffline: return "macOffline"
        case .suspended: return "suspended"
        case .revoked: return "revoked"
        case .pairingRequired: return "pairingRequired"
        }
    }

    private func stopSupervisor() async {
        LinkTrace.shared.mark("coordinator.stop.begin")
        defer { LinkTrace.shared.mark("coordinator.stop.end") }
        supervisor?.cancel()
        supervisor = nil
        fireRetry()
        if let connection {
            self.connection = nil
            await connection.close()
        }
        resumeReadyWaiters()
    }

    private func publish(_ next: RemoteLinkSnapshot) {
        LinkTrace.shared.mark("coordinator.\(Self.traceWord(next.state)).gen\(next.generation)")
        snapshot = next
        for observer in observers.values {
            observer.yield(next)
        }
    }

    /// Maps connector and control-plane failures onto the retry decision.
    static func classify(_ error: Error) -> RemoteLinkFailure {
        if let failure = error as? RemoteLinkFailure { return failure }
        if let error = error as? ControlPlaneError {
            switch error {
            case .rejected(let reason): return .revoked(reason)
            case .http, .transport, .malformedResponse: return .transient(error.message)
            }
        }
        if error is CancellationError { return .transient("cancelled") }
        return .transient(error.localizedDescription)
    }
}
