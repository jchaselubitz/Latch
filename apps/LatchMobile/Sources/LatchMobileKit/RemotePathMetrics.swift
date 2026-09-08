import Foundation

/// Which carrier the authenticated Remote Link selected.
public enum RemotePath: String, Sendable, Equatable {
    case local
    case relay

    public var label: String {
        switch self {
        case .local: return "Local network"
        case .relay: return "Relay"
        }
    }

    public var detail: String {
        switch self {
        case .local:
            return "Your Mac was found on this network and reached directly."
        case .relay:
            return "Connected through Latch's encrypted relay."
        }
    }
}

/// Thread-safe bridge from the native transport to the observable app model.
public final class RemotePathReporter: @unchecked Sendable {
    private let lock = NSLock()
    private var current: RemotePath?
    private var observer: (@Sendable (RemotePath?) -> Void)?
    private var tallyObserver: (@Sendable (RemotePathTally) -> Void)?
    private let metrics: any RemotePathMetricsStoring

    public init(metrics: any RemotePathMetricsStoring = UserDefaultsRemotePathMetricsStore()) {
        self.metrics = metrics
    }

    public var path: RemotePath? {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    public var tally: RemotePathTally { metrics.load() }

    public func observe(_ observer: @escaping @Sendable (RemotePath?) -> Void) {
        lock.lock()
        self.observer = observer
        let path = current
        lock.unlock()
        observer(path)
    }

    public func observeTally(_ observer: @escaping @Sendable (RemotePathTally) -> Void) {
        lock.lock()
        tallyObserver = observer
        lock.unlock()
        observer(metrics.load())
    }

    public func report(_ path: RemotePath?) {
        if let path { update { $0.record(path) } }
        lock.lock()
        guard path != current else {
            lock.unlock()
            return
        }
        current = path
        let observer = self.observer
        lock.unlock()
        observer?(path)
    }

    public func reportFailure() {
        update { $0.failures += 1 }
    }

    public func resetTally() {
        update { $0 = RemotePathTally() }
    }

    public func clear() {
        report(nil)
    }

    private func update(_ change: (inout RemotePathTally) -> Void) {
        lock.lock()
        var tally = metrics.load()
        change(&tally)
        metrics.save(tally)
        let observer = tallyObserver
        lock.unlock()
        observer?(tally)
    }
}

/// How the paired route has resolved on this phone, counted.
///
/// The Mac keeps the authoritative counters in its audit trail, but the Mac
/// is not the device that moves between networks. A field run — cellular to
/// home NAT, hotel Wi-Fi, a walk out of the door mid-terminal — happens on the
/// phone, and the person running it needs to read the outcome on the phone
/// rather than go back to a desk and export a diagnostics bundle.
///
/// Nothing here names a network, an address, an SSID, a carrier, or a moment.
/// It is four integers describing this phone's own connections, and it stays
/// on the phone: no call sends it anywhere.
public struct RemotePathTally: Sendable, Equatable, Codable {
    /// Channels opened over the local network.
    public var local: Int
    /// Historical baseline channels opened directly before Remote Link v1.
    public var direct: Int
    /// Channels opened through the relay.
    public var relay: Int
    /// Attempts that produced no channel at all, on any path.
    public var failures: Int

    public init(local: Int = 0, direct: Int = 0, relay: Int = 0, failures: Int = 0) {
        self.local = local
        self.direct = direct
        self.relay = relay
        self.failures = failures
    }

    /// Channels that opened, on any path.
    public var connections: Int { local + direct + relay }

    /// Attempts made, whether or not they produced a channel.
    public var attempts: Int { connections + failures }

    /// Share of opened channels that were relayed.
    ///
    /// `nil` before the first one: no connections is not the same measurement
    /// as no relaying, and a release gate reading "0%" should not be able to
    /// come from an empty counter.
    public var relayShare: Double? {
        guard connections > 0 else { return nil }
        return Double(relay) / Double(connections)
    }

    /// One line for the Settings row, or `nil` when nothing has been measured.
    public var summary: String? {
        guard attempts > 0 else { return nil }
        var parts: [String] = []
        if local > 0 { parts.append("Local \(local)") }
        if direct > 0 { parts.append("Direct \(direct)") }
        if relay > 0 { parts.append("Relay \(relay)") }
        if failures > 0 { parts.append("Failed \(failures)") }
        return parts.joined(separator: " · ")
    }

    mutating func record(_ path: RemotePath) {
        switch path {
        case .local: local += 1
        case .relay: relay += 1
        }
    }
}

/// Where the tally survives a launch.
public protocol RemotePathMetricsStoring: Sendable {
    func load() -> RemotePathTally
    func save(_ tally: RemotePathTally)
}

/// `UserDefaults`-backed storage, used by the app.
public struct UserDefaultsRemotePathMetricsStore: RemotePathMetricsStoring {
    // UserDefaults is documented thread-safe but isn't `Sendable` in the SDK;
    // `nonisolated(unsafe)` records that this is a deliberate, verified
    // exemption rather than an oversight.
    private nonisolated(unsafe) let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "remotePathTally") {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> RemotePathTally {
        guard
            let data = defaults.data(forKey: key),
            let tally = try? JSONDecoder().decode(RemotePathTally.self, from: data)
        else {
            // A tally that cannot be read starts again rather than failing the
            // route. It is a counter, and no connection depends on it.
            return RemotePathTally()
        }
        return tally
    }

    public func save(_ tally: RemotePathTally) {
        guard let data = try? JSONEncoder().encode(tally) else { return }
        defaults.set(data, forKey: key)
    }
}

/// In-memory storage for tests and for a build that should not persist.
public final class EphemeralRemotePathMetricsStore: RemotePathMetricsStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var tally = RemotePathTally()

    public init() {}

    public func load() -> RemotePathTally {
        lock.lock()
        defer { lock.unlock() }
        return tally
    }

    public func save(_ tally: RemotePathTally) {
        lock.lock()
        self.tally = tally
        lock.unlock()
    }
}
