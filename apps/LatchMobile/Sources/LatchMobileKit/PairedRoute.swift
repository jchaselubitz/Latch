import Foundation

/// Which network path the paired route is currently running over.
///
/// This is reported for the person's benefit, not for any decision the app
/// makes: "local" means the Mac was found on this network and reached
/// directly, "direct" means peer-to-peer through ICE, and "relay" means the
/// bytes are taking the TURN detour. Noise is identical on all three, so the
/// distinction is about speed and cost rather than trust.
public enum RemotePath: String, Sendable, Equatable {
    case local
    case direct
    case relay

    public var label: String {
        switch self {
        case .local: return "Local network"
        case .direct: return "Direct"
        case .relay: return "Relay"
        }
    }

    /// One sentence for the Settings footer, so the row is not a bare word.
    public var detail: String {
        switch self {
        case .local:
            return "Your Mac was found on this network and is reached directly."
        case .direct:
            return "Connected straight to your Mac across the internet."
        case .relay:
            return "Connected through Latch's relay because this network does not allow a direct one."
        }
    }
}

/// Where the transport writes the selected path and the UI reads it.
///
/// The transport runs off the main actor and the indicator lives on it, so
/// this is a small lock-guarded box with one observer rather than an
/// `@Observable` the transport would have to hop to touch.
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

    /// Every path this phone has resolved, across launches.
    public var tally: RemotePathTally { metrics.load() }

    /// Installs the single observer. Replacing it is deliberate: exactly one
    /// model displays this, and accumulating observers across route rebuilds
    /// would leak the dead ones.
    public func observe(_ observer: @escaping @Sendable (RemotePath?) -> Void) {
        lock.lock()
        self.observer = observer
        let path = current
        lock.unlock()
        observer(path)
    }

    /// Installs the single observer of the counters, on the same terms.
    public func observeTally(_ observer: @escaping @Sendable (RemotePathTally) -> Void) {
        lock.lock()
        tallyObserver = observer
        lock.unlock()
        observer(metrics.load())
    }

    public func report(_ path: RemotePath?) {
        // Counted before the change check, and only for an actual selection.
        // The indicator wants transitions — showing "Direct" twice is not two
        // events — but the rate wants channels: a route that opens four
        // channels over the relay relayed four times, and deduplicating that
        // to one would make a relay-heavy network look like a single blip.
        if let path { record(path) }
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

    /// Records an attempt that produced no channel on any path.
    ///
    /// The failures are the denominator. Without them a phone that relays once
    /// after nine dead attempts reads as "100% relay, all healthy", which is
    /// the opposite of what that network did.
    public func reportFailure() {
        update { $0.failures += 1 }
    }

    /// Clears the counters. The field-run protocol starts each scenario from
    /// zero, so this is deliberately reachable from Settings.
    public func resetTally() {
        update { $0 = RemotePathTally() }
    }

    public func clear() {
        report(nil)
    }

    private func record(_ path: RemotePath) {
        update { $0.record(path) }
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

/// What a remote (ICE) channel provider needs from the route that built it.
///
/// The ICE stack lives in `LatchTransportNative` because it is the only part
/// of the phone that links the shared Rust core. The sequencing above it —
/// try the LAN, check the Mac is actually present, then rendezvous — belongs
/// here, where it can be tested without an XCFramework.
public struct RemoteChannelContext: Sendable {
    public let record: PairedDeviceRecord
    public let signaling: any SignalingClient
    public let pathReporter: RemotePathReporter
    /// Called when the selected path changes underneath a live channel.
    /// Capabilities are re-read before application traffic resumes.
    public let onPathChange: @Sendable () async -> Void

    public init(
        record: PairedDeviceRecord,
        signaling: any SignalingClient,
        pathReporter: RemotePathReporter,
        onPathChange: @escaping @Sendable () async -> Void
    ) {
        self.record = record
        self.signaling = signaling
        self.pathReporter = pathReporter
        self.onPathChange = onPathChange
    }
}

public typealias RemoteChannelProviderFactory =
    @Sendable (RemoteChannelContext) -> any RemoteNoiseChannelProvider

/// Builds the phone's paired route: Bonjour first, then the control plane.
///
/// The order is a performance claim, not a security one. Both paths carry the
/// same pinned Noise session to the same listener, so nothing is trusted more
/// for having been found on the local network; the LAN is simply the shortest
/// way there when it exists.
public enum PairedGatewayRoute {
    /// How long to browse for the Mac before giving up on the local network.
    ///
    /// Bonjour reports an empty result set before multicast answers arrive, so
    /// a browse has to wait. It waits less when a remote path exists to fall
    /// through to: a Mac missed here is still reachable through its host
    /// candidates, while a phone on cellular would otherwise pay this delay in
    /// full before every connect.
    static let lanBrowseWithFallback = Duration.seconds(2)
    static let lanBrowseOnly = Duration.seconds(5)

    /// How many published addresses are dialled before falling through to ICE.
    ///
    /// A Mac publishes one candidate per interface, and a phone can usually
    /// route to none of them or exactly one. The cap bounds the worst case —
    /// several timeouts in a row — for a phone that is on none of those
    /// networks and whose real path is ICE.
    static let maxDirectTargets = 4

    /// The addresses in a presence record that can be dialled as plain TCP.
    ///
    /// The Mac's authenticated listener is TCP, so a `tcp` host candidate is
    /// one of its own interfaces: a LAN address, or a tailnet address, which is
    /// what makes a tailnet work here without ICE. Reflexive and relay
    /// candidates are excluded — they are UDP paths through someone else's
    /// server and there is nothing listening at them for TCP. A candidate with
    /// no metadata at all comes from a Mac that published only its listener,
    /// which is exactly this, so it counts.
    static func directTargets(in presence: PeerPresence) -> [NoiseTunnelTarget] {
        presence.candidates
            .filter { candidate in
                let type = candidate.type ?? "host"
                let proto = candidate.protocol ?? "tcp"
                return type == "host" && proto == "tcp"
            }
            .compactMap(target(for:))
            .prefix(maxDirectTargets)
            .map { $0 }
    }

    private static func target(for candidate: TransportCandidate) -> NoiseTunnelTarget? {
        let address = candidate.address
        let host: String
        let portText: Substring
        if address.hasPrefix("["), let close = address.firstIndex(of: "]") {
            host = String(address[address.index(after: address.startIndex)..<close])
            guard address.index(after: close) < address.endIndex else { return nil }
            portText = address[address.index(close, offsetBy: 2)...]
        } else {
            guard let colon = address.lastIndex(of: ":") else { return nil }
            host = String(address[..<colon])
            portText = address[address.index(after: colon)...]
        }
        guard let port = UInt16(portText) else { return nil }
        return try? NoiseTunnelTarget(host: host, port: port)
    }

    public typealias Factory = @Sendable (PairedDeviceRecord) async throws -> LatchGateway

    /// - Parameters:
    ///   - remoteProvider: the ICE channel provider, absent in builds and
    ///     tests that do not link the native transport. Without it the route
    ///     is the local network or nothing.
    public static func factory(
        identityStore: any DeviceIdentityStoring = KeychainDeviceIdentityStore(),
        pathReporter: RemotePathReporter = RemotePathReporter(),
        discoverLAN: (@Sendable (PairedDeviceRecord, Duration) async throws -> NoiseTunnelTarget?)? = nil,
        signalingFactory: @escaping @Sendable (URL) -> any SignalingClient = {
            HTTPControlPlaneClient(baseURL: $0)
        },
        remoteProvider: RemoteChannelProviderFactory? = nil
    ) -> Factory {
        let discover = discoverLAN ?? { record, duration in
            try await BonjourMacDiscovery()
                .candidates(matching: record.mac.publicKey, for: duration)
                .first
        }
        return { record in
            let signaling = record.controlPlane.map(signalingFactory)
            let remote = signaling.flatMap { signaling in
                remoteProvider.map { make in (signaling: signaling, make: make) }
            }
            let lanTarget = try? await discover(
                record,
                remote == nil ? lanBrowseOnly : lanBrowseWithFallback
            )
            guard let remote else {
                // No native transport, or a pairing with no control plane at
                // all: the local network is the only route this build has.
                guard let lanTarget else { throw NoiseTunnelError.macNotReachable }
                pathReporter.report(.local)
                let transport = try await NoiseTunnelGatewayTransport.start(
                    target: lanTarget,
                    pairedDevice: record,
                    identityStore: identityStore
                )
                return LatchGateway(transport: transport)
            }

            let rediscovery = GatewayRediscovery()
            let provider = FallbackChannelProvider(
                lan: lanTarget.map { LANRemoteNoiseChannelProvider(target: $0) },
                remote: remote.make(
                    RemoteChannelContext(
                        record: record,
                        signaling: remote.signaling,
                        pathReporter: pathReporter,
                        onPathChange: { await rediscovery.run() }
                    )
                ),
                record: record,
                signaling: remote.signaling,
                pathReporter: pathReporter
            )
            let transport = try await NoiseTunnelGatewayTransport.start(
                channelProvider: provider,
                pairedDevice: record,
                identityStore: identityStore
            )
            let gateway = LatchGateway(transport: transport)
            await rediscovery.install(gateway)
            return gateway
        }
    }
}

/// The local network first, the control plane second.
///
/// A LAN target that fails is not a dead end: the Bonjour record may be stale,
/// the phone may have moved between the browse and the connect, or the Mac may
/// be answering on an interface this phone cannot route to. Falling through
/// costs one failed TCP connect and turns those cases into a working session
/// instead of an error.
final class FallbackChannelProvider: RemoteNoiseChannelProvider, @unchecked Sendable {
    private let lan: (any RemoteNoiseChannelProvider)?
    private let remote: any RemoteNoiseChannelProvider
    private let record: PairedDeviceRecord
    private let signaling: any SignalingClient
    private let pathReporter: RemotePathReporter
    private let reachability = RemoteReachabilityGate()

    init(
        lan: (any RemoteNoiseChannelProvider)?,
        remote: any RemoteNoiseChannelProvider,
        record: PairedDeviceRecord,
        signaling: any SignalingClient,
        pathReporter: RemotePathReporter
    ) {
        self.lan = lan
        self.remote = remote
        self.record = record
        self.signaling = signaling
        self.pathReporter = pathReporter
    }

    func openChannel() async throws -> any RemoteNoiseChannel {
        if let lan {
            do {
                let channel = try await lan.openChannel()
                pathReporter.report(.local)
                return channel
            } catch {
                // Fall through. The remote path reports its own selection.
            }
        }
        // Presence is read before gathering rather than after: a Mac that is
        // asleep or not running Latch has no ICE agent to answer connectivity
        // checks, and the phone should say so in one round trip instead of
        // spending a gathering pass and a rendezvous timeout to find out.
        //
        // The same read supplies the addresses the Mac's listener is bound on,
        // which is the whole tailnet story: a Tailscale address is one of them,
        // and dialling it is an ordinary TCP connect. No ICE, no rendezvous,
        // and no relay can be involved in a path that never leaves the tailnet.
        let direct: [NoiseTunnelTarget]
        do {
            direct = try await reachability.confirm {
                try await signaling.macPresence(for: record)
            }
        } catch {
            // A Mac that is asleep is a failed attempt like any other. Leaving
            // it out would let a phone that never reached its Mac at all show
            // a clean record.
            pathReporter.reportFailure()
            throw error
        }
        for target in direct {
            guard let channel = try? await LANRemoteNoiseChannelProvider(target: target)
                .openChannel() else { continue }
            pathReporter.report(.direct)
            await reachability.reached()
            return channel
        }
        do {
            let channel = try await remote.openChannel()
            await reachability.reached()
            return channel
        } catch {
            await reachability.lost()
            pathReporter.reportFailure()
            throw error
        }
    }
}

/// Remembers that the Mac answered, so the presence read stays a diagnostic
/// for the first attempt rather than a round trip on every request.
///
/// The transport opens one channel per loopback request, so an unconditional
/// check would put a control-plane call in front of each of them. A failure
/// re-arms it: that is exactly when "is the Mac even there?" is worth asking
/// again.
actor RemoteReachabilityGate {
    private var reachedOnce = false
    /// The Mac's own interface addresses, from the last presence read. Kept so
    /// the second and later channels of a route dial the tailnet directly
    /// without another control-plane round trip.
    private var directTargets: [NoiseTunnelTarget] = []

    /// Confirms the Mac is present and reports the addresses it can be dialled
    /// at directly. Inside the gate this answers from the last read.
    func confirm(_ read: () async throws -> PeerPresence) async throws -> [NoiseTunnelTarget] {
        guard !reachedOnce else { return directTargets }
        let presence = try await read()
        guard presence.online else { throw ControlPlaneError.macOffline }
        directTargets = PairedGatewayRoute.directTargets(in: presence)
        return directTargets
    }

    func reached() {
        reachedOnce = true
    }

    func lost() {
        reachedOnce = false
        // The addresses came with the presence record that just proved wrong.
        // A Mac that moved networks publishes new ones on the next read.
        directTargets = []
    }
}

/// Re-reads capabilities when the selected path changes underneath a live
/// gateway, before the provider releases the triggering channel to traffic.
actor GatewayRediscovery {
    private var gateway: LatchGateway?

    func install(_ gateway: LatchGateway) {
        self.gateway = gateway
    }

    func run() async {
        guard let gateway else { return }
        await gateway.invalidateDiscovery()
        _ = try? await gateway.discover()
    }
}
