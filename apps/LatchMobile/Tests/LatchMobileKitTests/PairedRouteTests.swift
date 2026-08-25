import Foundation
import Network
import XCTest

@testable import LatchMobileKit

/// The phone's route selection: the local network first, the control plane
/// second, and one honest sentence when the Mac is simply not there.
@MainActor
final class PairedRouteTests: XCTestCase {
    private let controlPlane = URL(string: "https://control.example")!

    // MARK: - Order

    func testLocalNetworkIsUsedFirstAndNeverTouchesTheControlPlane() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let remote = RecordingChannelProvider()
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let provider = FallbackChannelProvider(
            lan: RecordingChannelProvider(),
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: reporter
        )

        _ = try await provider.openChannel()

        XCTAssertEqual(reporter.path, .local)
        let opens = await remote.opens
        XCTAssertEqual(opens, 0, "ICE must not run while the LAN answers")
        let presenceReads = await signaling.presenceReads
        XCTAssertEqual(presenceReads, 0, "a LAN session needs no control plane at all")
    }

    /// A Bonjour record can be stale, and the phone can move between the
    /// browse and the connect. Neither should end the attempt.
    func testAFailedLocalConnectFallsThroughToTheControlPlane() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let remote = RecordingChannelProvider()
        let provider = FallbackChannelProvider(
            lan: RecordingChannelProvider(failure: NoiseTunnelError.macNotReachable),
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        _ = try await provider.openChannel()

        let presenceReads = await signaling.presenceReads
        XCTAssertEqual(presenceReads, 1)
        let opens = await remote.opens
        XCTAssertEqual(opens, 1)
    }

    func testNoLocalServiceGoesStraightToTheControlPlane() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let remote = RecordingChannelProvider()
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        _ = try await provider.openChannel()

        let opens = await remote.opens
        XCTAssertEqual(opens, 1)
    }

    /// One channel is opened per loopback request. The presence read is a
    /// diagnostic for the first attempt, not a control-plane round trip in
    /// front of every one of them.
    func testPresenceIsCheckedOnceWhileTheMacKeepsAnswering() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: RecordingChannelProvider(),
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        _ = try await provider.openChannel()
        _ = try await provider.openChannel()

        let presenceReads = await signaling.presenceReads
        XCTAssertEqual(presenceReads, 1)
    }

    /// A failed attempt re-arms it, because that is when the question is
    /// worth asking again.
    func testAFailedRemoteAttemptRestoresThePresenceCheck() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: RecordingChannelProvider(failure: NoiseTunnelError.closed),
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        _ = try? await provider.openChannel()
        _ = try? await provider.openChannel()

        let presenceReads = await signaling.presenceReads
        XCTAssertEqual(presenceReads, 2)
    }

    // MARK: - The Mac is not there

    func testAnAbsentPresenceStopsBeforeGatheringAndNamesTheCause() async throws {
        let signaling = RouteSignalingStub(presence: .offline)
        let remote = RecordingChannelProvider()
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        do {
            _ = try await provider.openChannel()
            XCTFail("expected macOffline")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .macOffline)
            XCTAssertEqual(error.message, "Your Mac is asleep or Latch is not running.")
        }
        let opens = await remote.opens
        XCTAssertEqual(opens, 0, "no gathering pass is spent on a Mac that is not present")
    }

    /// The service reports the same condition on the rendezvous call. Both
    /// arrive as one sentence rather than as a status code.
    func testARendezvousOfflineAnswerCarriesTheSameSentence() async throws {
        let signaling = RouteSignalingStub(presence: .online)
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: RecordingChannelProvider(failure: ControlPlaneError.macOffline),
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        do {
            _ = try await provider.openChannel()
            XCTFail("expected macOffline")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error.message, "Your Mac is asleep or Latch is not running.")
        }
    }

    /// The reason has to survive the phone's own loopback shim. It is
    /// delivered as a 502 there, and a person reading "502" learns nothing.
    func testAnUnreachableMacReachesTheCallerAsOneSentence() async throws {
        let identities = MemoryDeviceIdentityStore()
        try identities.loadOrCreate()
        let transport = try await NoiseTunnelGatewayTransport.start(
            channelProvider: RecordingChannelProvider(failure: ControlPlaneError.macOffline),
            pairedDevice: record(),
            identityStore: identities
        )
        defer { transport.stop() }

        do {
            _ = try await LatchGateway(transport: transport).discover()
            XCTFail("expected the tunnel failure to surface")
        } catch let error as LatchError {
            XCTAssertEqual(error.message, "Your Mac is asleep or Latch is not running.")
        }
    }

    // MARK: - Route assembly

    func testWithoutTheNativeTransportAMissedBonjourBrowseIsTheEndOfTheRoute() async {
        let factory = PairedGatewayRoute.factory(
            identityStore: MemoryDeviceIdentityStore(),
            discoverLAN: { _, _ in nil }
        )
        do {
            _ = try await factory(record())
            XCTFail("expected macNotReachable")
        } catch let error as NoiseTunnelError {
            XCTAssertEqual(error, .macNotReachable)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    /// The browse is shortened when there is somewhere to fall through to, so
    /// a phone on cellular does not pay the full local-network wait first.
    func testTheBrowseIsShorterWhenTheControlPlaneCanTakeOver() async throws {
        let durations = DurationRecorder()
        let identities = MemoryDeviceIdentityStore()
        try identities.loadOrCreate()
        let withFallback = PairedGatewayRoute.factory(
            identityStore: identities,
            discoverLAN: { _, duration in
                await durations.record(duration)
                return nil
            },
            signalingFactory: { _ in RouteSignalingStub(presence: .online) },
            remoteProvider: { _ in RecordingChannelProvider() }
        )
        _ = try await withFallback(record())

        let lanOnly = PairedGatewayRoute.factory(
            identityStore: identities,
            discoverLAN: { _, duration in
                await durations.record(duration)
                return nil
            }
        )
        _ = try? await lanOnly(record())

        let recorded = await durations.durations
        XCTAssertEqual(recorded, [PairedGatewayRoute.lanBrowseWithFallback, PairedGatewayRoute.lanBrowseOnly])
    }

    // MARK: - The indicator

    func testTheReportedPathReachesTheModel() async throws {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let model = AppModel(
            storage: MemoryLinkStorage(),
            pathReporter: reporter
        )
        XCTAssertNil(model.remotePath)

        reporter.report(.relay)
        await Task.yield()
        XCTAssertEqual(model.remotePath, .relay)

        reporter.clear()
        await Task.yield()
        XCTAssertNil(model.remotePath)
    }

    func testTheReporterOnlyAnnouncesRealChanges() {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let observed = Counter()
        reporter.observe { _ in observed.increment() }
        XCTAssertEqual(observed.value, 1, "an observer is told the current path when it installs")

        reporter.report(.direct)
        reporter.report(.direct)
        XCTAssertEqual(observed.value, 2)
    }

    // MARK: - Helpers

    // MARK: - Path counters

    func testAFailedRemoteAttemptIsCounted() async {
        let signaling = RouteSignalingStub(presence: .online)
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: RecordingChannelProvider(failure: NoiseTunnelError.macNotReachable),
            record: record(),
            signaling: signaling,
            pathReporter: reporter
        )

        _ = try? await provider.openChannel()

        XCTAssertEqual(reporter.tally.failures, 1)
        XCTAssertEqual(reporter.tally.connections, 0)
    }

    /// A phone that never reached its Mac at all must not show a clean record.
    func testASleepingMacIsCountedAsAFailedAttempt() async {
        let signaling = RouteSignalingStub(presence: .offline)
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: RecordingChannelProvider(),
            record: record(),
            signaling: signaling,
            pathReporter: reporter
        )

        do {
            _ = try await provider.openChannel()
            XCTFail("an offline Mac has no route")
        } catch {
            XCTAssertEqual(error as? ControlPlaneError, .macOffline)
        }
        XCTAssertEqual(reporter.tally.failures, 1)
    }

    // MARK: - Direct addresses from presence

    /// The Mac's authenticated listener is TCP, so its `tcp` host candidates
    /// are its own interfaces — a tailnet address among them. Those are
    /// dialable directly; a reflexive UDP candidate is not, and nothing is
    /// listening at it for TCP.
    func testPresenceHostCandidatesBecomeDirectTCPTargetsIncludingATailnetOne() {
        let presence = PeerPresence(
            deviceId: "dev_mac",
            online: true,
            candidates: [
                candidate("192.168.1.20:49221", type: "host", proto: "tcp"),
                candidate("100.64.0.7:49221", type: "host", proto: "tcp"),
                candidate("[fd7a:115c:a1e0::1]:49221", type: "host", proto: "tcp"),
                candidate("203.0.113.9:52000", type: "srflx", proto: "udp"),
                candidate("192.168.1.20:52000", type: "host", proto: "udp"),
            ]
        )

        XCTAssertEqual(PairedGatewayRoute.directTargets(in: presence).count, 3)

        // A Mac that published only its listener sends no metadata at all.
        // That candidate is the TCP listener, so it still counts.
        let legacy = PeerPresence(
            deviceId: "dev_mac",
            online: true,
            candidates: [TransportCandidate(address: "192.168.1.20:49221", expiresAt: 99)]
        )
        XCTAssertEqual(PairedGatewayRoute.directTargets(in: legacy).count, 1)

        // Malformed addresses are dropped rather than dialled.
        let broken = PeerPresence(
            deviceId: "dev_mac",
            online: true,
            candidates: [
                candidate("192.168.1.20", type: "host", proto: "tcp"),
                candidate("192.168.1.20:0", type: "host", proto: "tcp"),
            ]
        )
        XCTAssertTrue(PairedGatewayRoute.directTargets(in: broken).isEmpty)
    }

    /// A tailnet needs no ICE: the address is published as presence, the phone
    /// dials it, and the same pinned Noise session runs over the socket. ICE is
    /// never asked for a channel.
    func testAReachablePresenceAddressIsDialledBeforeICE() async throws {
        let listener = try TCPProbeListener()
        defer { listener.stop() }
        let signaling = RouteSignalingStub(
            presence: .online,
            candidates: [candidate("127.0.0.1:\(listener.port)", type: "host", proto: "tcp")]
        )
        let remote = RecordingChannelProvider()
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: reporter
        )

        _ = try await provider.openChannel()

        XCTAssertEqual(reporter.path, .direct)
        let opens = await remote.opens
        XCTAssertEqual(opens, 0, "a reachable published address must not fall through to ICE")
    }

    /// An address the phone cannot route to — the common case, since a Mac
    /// publishes every interface it has — costs one refused connect and then
    /// gives way to ICE.
    func testAnUnreachablePresenceAddressFallsThroughToICE() async throws {
        let signaling = RouteSignalingStub(
            presence: .online,
            // A port nothing is listening on: the connect is refused rather
            // than left to time out, which is what keeps this test quick.
            candidates: [candidate("127.0.0.1:9", type: "host", proto: "tcp")]
        )
        let remote = RecordingChannelProvider()
        let provider = FallbackChannelProvider(
            lan: nil,
            remote: remote,
            record: record(),
            signaling: signaling,
            pathReporter: RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        )

        _ = try await provider.openChannel()

        let opens = await remote.opens
        XCTAssertEqual(opens, 1)
    }

    /// The provider must not hand back a channel whose connect is still in
    /// flight. A caller with another target to try needs the failure here,
    /// while there is still somewhere to fall through to.
    func testOpeningATCPChannelWaitsForTheConnection() async throws {
        let listener = try TCPProbeListener()
        defer { listener.stop() }
        let live = LANRemoteNoiseChannelProvider(
            target: try NoiseTunnelTarget(host: "127.0.0.1", port: listener.port)
        )
        let channel = try await live.openChannel()
        await channel.close()

        let dead = LANRemoteNoiseChannelProvider(
            target: try NoiseTunnelTarget(host: "127.0.0.1", port: 9),
            connectTimeout: .seconds(2)
        )
        do {
            _ = try await dead.openChannel()
            XCTFail("a refused connect must surface as a failure to open the channel")
        } catch {}
    }

    private func candidate(
        _ address: String,
        type: String?,
        proto: String?
    ) -> TransportCandidate {
        TransportCandidate(
            address: address,
            expiresAt: 99,
            type: type,
            priority: 2_130_706_431,
            foundation: "f",
            component: 1,
            protocol: proto,
            tcpType: proto == "tcp" ? "passive" : nil
        )
    }

    private func record() -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "dev_phone",
            name: "Test iPhone",
            devicePublicKey: String(repeating: "2", count: 64),
            mac: PairedMac(
                deviceId: "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                publicKey: String(repeating: "a", count: 64),
                name: "Studio Mac"
            ),
            permission: .interact,
            phrase: "sable-apple-maple-garnet-maple-flint",
            controlPlane: controlPlane,
            accessToken: "phone-access-token"
        )
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    var value: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func increment() {
        lock.lock()
        count += 1
        lock.unlock()
    }
}

private actor DurationRecorder {
    private(set) var durations: [Duration] = []

    func record(_ duration: Duration) {
        durations.append(duration)
    }
}

private actor RecordingChannelProvider: RemoteNoiseChannelProvider {
    private let failure: (any Error)?
    private(set) var opens = 0

    init(failure: (any Error)? = nil) {
        self.failure = failure
    }

    func openChannel() async throws -> any RemoteNoiseChannel {
        opens += 1
        if let failure { throw failure }
        return DeadChannel()
    }
}

/// The route under test never reads or writes; it only decides which provider
/// opens the channel.
private struct DeadChannel: RemoteNoiseChannel {
    func readFrame() async throws -> Data { throw NoiseTunnelError.closed }
    func writeFrame(_ frame: Data) async throws { throw NoiseTunnelError.closed }
    func close() async {}
}

private actor RouteSignalingStub: SignalingClient {
    enum Presence {
        case online
        case offline
    }

    private let online: Bool
    private let candidates: [TransportCandidate]
    private(set) var presenceReads = 0

    init(presence: Presence, candidates: [TransportCandidate] = []) {
        online = presence == .online
        self.candidates = candidates
    }

    func presence(deviceId: String, accessToken: String) async throws -> PeerPresence {
        presenceReads += 1
        return PeerPresence(deviceId: deviceId, online: online, candidates: candidates)
    }

    func publishPresence(
        candidates: [TransportCandidate],
        iceUfrag: String?,
        icePwd: String?,
        accessToken: String
    ) async throws -> PublishedPresence {
        PublishedPresence(deviceId: "dev_phone", expiresAt: 0, ttlSeconds: 0)
    }

    func clearPresence(accessToken: String) async throws {}

    func offerRendezvous(
        targetDeviceId: String,
        candidates: [TransportCandidate],
        iceUfrag: String?,
        icePwd: String?,
        accessToken: String,
        requestId: String,
        expiresAt: UInt64?
    ) async throws -> RendezvousAnswer {
        throw ControlPlaneError.macOffline
    }

    func collectOffers(accessToken: String) async throws -> [RendezvousOffer] { [] }

    func iceServers(accessToken: String) async throws -> [IceServer] { [] }

    func turnCredentials(peerDeviceId: String, accessToken: String) async throws -> TurnCredentials {
        throw ControlPlaneError.relayDisabled("")
    }
}

/// A TCP listener that accepts and holds connections, so a dial can be told
/// apart from a dial that only looked like one.
private final class TCPProbeListener: @unchecked Sendable {
    private let listener: NWListener
    private let lock = NSLock()
    private let accepted = AcceptedConnections()
    let port: UInt16

    init() throws {
        let listener = try NWListener(using: .tcp, on: .any)
        self.listener = listener
        let ready = DispatchSemaphore(value: 0)
        let accepted = self.accepted
        listener.stateUpdateHandler = { state in
            if case .ready = state { ready.signal() }
        }
        listener.newConnectionHandler = { connection in
            connection.start(queue: DispatchQueue(label: "dev.cooperativ.latch.probe-connection"))
            accepted.append(connection)
        }
        listener.start(queue: DispatchQueue(label: "dev.cooperativ.latch.probe-listener"))
        guard ready.wait(timeout: .now() + 5) == .success, let bound = listener.port else {
            listener.cancel()
            throw NoiseTunnelError.listenerUnavailable
        }
        port = bound.rawValue
    }

    func stop() {
        accepted.cancelAll()
        listener.cancel()
    }
}

private final class AcceptedConnections: @unchecked Sendable {
    private let lock = NSLock()
    private var connections: [NWConnection] = []

    func append(_ connection: NWConnection) {
        lock.lock()
        connections.append(connection)
        lock.unlock()
    }

    func cancelAll() {
        lock.lock()
        let open = connections
        connections = []
        lock.unlock()
        open.forEach { $0.cancel() }
    }
}
