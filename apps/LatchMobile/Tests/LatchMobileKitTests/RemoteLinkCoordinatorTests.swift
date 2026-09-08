import XCTest

@testable import LatchMobileKit

/// A scripted link connector: each connect attempt consumes one script entry
/// and either fails with a classified failure or yields a connection the test
/// can drop at will.
final class ScriptedConnector: RemoteLinkConnecting, @unchecked Sendable {
    enum Step {
        case connect(RemotePath)
        case fail(RemoteLinkFailure)
        case failWith(Error)
    }

    final class Connection: RemoteLinkConnection, @unchecked Sendable {
        let path: RemotePath
        let grantRevision: UInt64 = 1
        let timings = RemoteLinkStageTimings(admissionMs: 40, connectMs: 120, peerWaitMs: 5, authenticateMs: 30)
        private let lock = NSLock()
        private var closedFlag = false
        private var waiters: [CheckedContinuation<Void, Never>] = []
        var channels = 0
        var closedByOwner = false

        init(path: RemotePath) { self.path = path }

        var isClosed: Bool { lock.withLock { closedFlag } }

        func openGatewayChannel() async throws -> any AuthenticatedGatewayChannel {
            lock.withLock { channels += 1 }
            return NullChannel()
        }

        func waitClosed() async {
            let already: Bool = lock.withLock { closedFlag }
            if already { return }
            await withCheckedContinuation { continuation in
                lock.withLock { waiters.append(continuation) }
            }
        }

        func close() async {
            lock.withLock { closedByOwner = true }
            drop()
        }

        /// Transport loss from outside: the link dies, the owner notices.
        func drop() {
            let pending: [CheckedContinuation<Void, Never>] = lock.withLock {
                closedFlag = true
                let waiters = self.waiters
                self.waiters = []
                return waiters
            }
            pending.forEach { $0.resume() }
        }
    }

    final class NullChannel: AuthenticatedGatewayChannel, @unchecked Sendable {
        func read() async throws -> Data { throw RemoteLinkTransportError.closed }
        func write(_ bytes: Data) async throws {}
        func close() async {}
    }

    private let lock = NSLock()
    private var script: [Step]
    private(set) var attempts = 0
    private(set) var options: [RemoteLinkConnectOptions] = []
    private(set) var connections: [Connection] = []
    /// Resolves each time an attempt starts, so tests can wait for it.
    private var attemptWaiters: [CheckedContinuation<Void, Never>] = []

    init(_ script: [Step]) { self.script = script }

    var latest: Connection? { lock.withLock { connections.last } }

    func connect(record: PairedDeviceRecord, options: RemoteLinkConnectOptions) async throws -> any RemoteLinkConnection {
        let step: Step? = lock.withLock {
            attempts += 1
            self.options.append(options)
            let waiters = attemptWaiters
            attemptWaiters = []
            waiters.forEach { $0.resume() }
            return script.isEmpty ? nil : script.removeFirst()
        }
        switch step {
        case .connect(let path)?:
            let connection = Connection(path: path)
            lock.withLock { connections.append(connection) }
            return connection
        case .fail(let failure)?:
            throw failure
        case .failWith(let error)?:
            throw error
        case nil:
            throw RemoteLinkFailure.transient("script exhausted")
        }
    }

    func nextAttempt() async {
        await withCheckedContinuation { continuation in
            lock.withLock { attemptWaiters.append(continuation) }
        }
    }
}

/// A controllable clock and sleeper so backoff is asserted, not waited for.
final class VirtualTime: @unchecked Sendable {
    private let lock = NSLock()
    private var now = Date(timeIntervalSince1970: 1_800_000_000)
    private(set) var sleeps: [Duration] = []
    private var sleepers: [(Duration, CheckedContinuation<Void, Error>)] = []

    var date: Date { lock.withLock { now } }

    func clock() -> Date { date }

    func sleep(_ duration: Duration) async throws {
        try await withCheckedThrowingContinuation { continuation in
            lock.withLock {
                sleeps.append(duration)
                sleepers.append((duration, continuation))
            }
        }
    }

    /// Advances time and wakes every sleeper that has waited long enough.
    func advance(by seconds: TimeInterval) {
        let woken: [CheckedContinuation<Void, Error>] = lock.withLock {
            now = now.addingTimeInterval(seconds)
            let due = sleepers.filter { Self.seconds($0.0) <= seconds }
            sleepers.removeAll { Self.seconds($0.0) <= seconds }
            return due.map(\.1)
        }
        woken.forEach { $0.resume() }
    }

    /// Wakes the earliest sleeper regardless of duration.
    func wakeAll() {
        let woken: [CheckedContinuation<Void, Error>] = lock.withLock {
            let all = sleepers.map(\.1)
            sleepers = []
            return all
        }
        woken.forEach { $0.resume() }
    }

    static func seconds(_ duration: Duration) -> TimeInterval {
        Double(duration.components.seconds) + Double(duration.components.attoseconds) / 1e18
    }
}

final class RemoteLinkCoordinatorTests: XCTestCase {
    private func record() -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "dev_phone", name: "Phone", devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: "dev_mac", publicKey: String(repeating: "22", count: 32), name: "Mac"),
            permission: .control, comparison: "0000 0000 0000 0000",
            controlPlane: URL(string: "https://control.example")!, accessToken: "token"
        )
    }

    private func settle() async {
        for _ in 0..<50 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(20))
    }

    private func waitFor(_ coordinator: RemoteLinkCoordinator, _ predicate: @escaping (RemoteLinkState) -> Bool) async -> RemoteLinkState {
        for _ in 0..<200 {
            let state = await coordinator.current.state
            if predicate(state) { return state }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return await coordinator.current.state
    }

    func testBackoffIsFullJitterBetweenTheFloorAndFifteenSeconds() {
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.ceiling(forAttempt: 0)), 0.25, accuracy: 0.001)
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.ceiling(forAttempt: 3)), 2.0, accuracy: 0.001)
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.ceiling(forAttempt: 6)), 15.0, accuracy: 0.001)
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.ceiling(forAttempt: 40)), 15.0, accuracy: 0.001)
        // Jitter never exceeds the ceiling and can go to zero.
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.delay(forAttempt: 6, random: 1)), 15.0, accuracy: 0.001)
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.delay(forAttempt: 6, random: 0)), 0, accuracy: 0.001)
        XCTAssertEqual(VirtualTime.seconds(RemoteLinkBackoff.delay(forAttempt: 2, random: 0.5)), 0.5, accuracy: 0.001)
    }

    func testOneOwnerReconnectsWithBackoffAndAForegroundEventCutsItShort() async {
        let connector = ScriptedConnector([.connect(.relay), .fail(.transient("blip")), .connect(.local)])
        let time = VirtualTime()
        let coordinator = RemoteLinkCoordinator(connector: connector, clock: time.clock, sleep: time.sleep, random: { 1 })
        await coordinator.start(record: record())
        let ready = await waitFor(coordinator) { $0 == .ready }
        XCTAssertEqual(ready, .ready)
        let first = await coordinator.current
        XCTAssertEqual(first.generation, 1)
        XCTAssertEqual(first.path, .relay)
        XCTAssertEqual(first.timings?.linkReadyMs, 195)

        // The link dies almost immediately: attempt 0 backoff is 250 ms at
        // random=1, and the next attempt fails, so the second backoff is 500 ms.
        connector.latest?.drop()
        let backingOff = await waitFor(coordinator) { if case .backoff = $0 { return true }; return false }
        guard case .backoff(let attempt, _, _) = backingOff else { return XCTFail("expected backoff, got \(backingOff)") }
        XCTAssertEqual(attempt, 1)
        XCTAssertEqual(time.sleeps.last, .seconds(0.25))
        time.advance(by: 0.25)
        let second = await waitFor(coordinator) { if case .backoff(2, _, _) = $0 { return true }; return false }
        guard case .backoff(2, _, let reason) = second else { return XCTFail("expected second backoff, got \(second)") }
        XCTAssertEqual(reason, "blip")
        XCTAssertEqual(time.sleeps.last, .seconds(0.5))

        // Foreground: one immediate retry through the same owner, no second
        // loop. The next scripted step connects over LAN as generation 2.
        await coordinator.retryImmediately()
        let recovered = await waitFor(coordinator) { $0 == .ready }
        XCTAssertEqual(recovered, .ready)
        let snapshot = await coordinator.current
        XCTAssertEqual(snapshot.generation, 2)
        XCTAssertEqual(snapshot.path, .local)
        XCTAssertEqual(connector.attempts, 3, "exactly one attempt per schedule slot; nothing raced it")
        await coordinator.stop()
    }

    func testAuthenticationAndRevocationStopAutomaticRetry() async {
        for (failure, expected) in [
            (RemoteLinkFailure.authentication("pin mismatch"), RemoteLinkState.pairingRequired("pin mismatch")),
            (RemoteLinkFailure.revoked("unpaired"), RemoteLinkState.revoked("unpaired")),
        ] {
            let connector = ScriptedConnector([.fail(failure), .connect(.relay)])
            let time = VirtualTime()
            let coordinator = RemoteLinkCoordinator(connector: connector, clock: time.clock, sleep: time.sleep)
            await coordinator.start(record: record())
            let state = await waitFor(coordinator) { $0.isTerminal }
            XCTAssertEqual(state, expected)
            await settle()
            time.wakeAll()
            await settle()
            XCTAssertEqual(connector.attempts, 1, "a terminal failure must not be retried")
            do {
                _ = try await coordinator.openGatewayChannel()
                XCTFail("a channel opened on a stopped owner")
            } catch let error as RemoteLinkCoordinator.ChannelError {
                XCTAssertTrue(error == .notRunning || error == .unavailable(expected))
            } catch {
                XCTFail("unexpected \(error)")
            }
            await coordinator.stop()
        }
    }

    func testMacOfflineKeepsRetryingUnderItsOwnLabel() async {
        let connector = ScriptedConnector([.fail(.macOffline), .connect(.relay)])
        let time = VirtualTime()
        let coordinator = RemoteLinkCoordinator(connector: connector, clock: time.clock, sleep: time.sleep, random: { 1 })
        await coordinator.start(record: record())
        let offline = await waitFor(coordinator) { if case .macOffline = $0 { return true }; return false }
        guard case .macOffline(let nextRetryAt) = offline else { return XCTFail("expected macOffline, got \(offline)") }
        XCTAssertEqual(nextRetryAt.timeIntervalSince(time.date), 0.25, accuracy: 0.01)
        time.advance(by: 0.25)
        let ready = await waitFor(coordinator) { $0 == .ready }
        XCTAssertEqual(ready, .ready)
        await coordinator.stop()
    }

    func testSuspendClosesTheLinkAndResumeReconnectsAsANewGeneration() async {
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        let coordinator = RemoteLinkCoordinator(connector: connector)
        await coordinator.start(record: record())
        _ = await waitFor(coordinator) { $0 == .ready }
        let first = connector.latest
        await coordinator.suspend()
        let suspendedState = await coordinator.current.state
        XCTAssertEqual(suspendedState, .suspended)
        XCTAssertEqual(first?.closedByOwner, true, "suspending releases the native link")
        do {
            _ = try await coordinator.openGatewayChannel()
            XCTFail("a suspended owner must not open streams")
        } catch {}
        await coordinator.resume()
        _ = await waitFor(coordinator) { $0 == .ready }
        let resumedGeneration = await coordinator.current.generation
        XCTAssertEqual(resumedGeneration, 2)
        XCTAssertNotIdentical(connector.latest, first)
        await coordinator.stop()
    }

    func testChannelRequestsWaitForAConnectingLinkInsteadOfOpeningTheirOwn() async {
        let connector = ScriptedConnector([.fail(.transient("first")), .connect(.relay)])
        let time = VirtualTime()
        let coordinator = RemoteLinkCoordinator(connector: connector, clock: time.clock, sleep: time.sleep, random: { 1 })
        await coordinator.start(record: record())
        _ = await waitFor(coordinator) { if case .backoff = $0 { return true }; return false }
        let opening = Task { try await coordinator.openGatewayChannel() }
        await settle()
        // Only the owner connects; the waiting channel request did not add an attempt.
        XCTAssertEqual(connector.attempts, 1)
        time.advance(by: 0.25)
        let channel = try? await opening.value
        XCTAssertNotNil(channel)
        XCTAssertEqual(connector.attempts, 2)
        XCTAssertEqual(connector.latest?.channels, 1)
        await coordinator.stop()
    }

    func testAFailedProbeAfterANetworkChangeReplacesTheLink() async {
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        let coordinator = RemoteLinkCoordinator(connector: connector, random: { 0 })
        await coordinator.setProbe { false }
        await coordinator.start(record: record())
        _ = await waitFor(coordinator) { $0 == .ready }
        let first = connector.latest
        await coordinator.retryImmediately()
        let replaced = await waitFor(coordinator) { $0 == .ready && connector.attempts == 2 }
        XCTAssertEqual(replaced, .ready)
        XCTAssertEqual(first?.closedByOwner, true, "a link that fails its probe is closed, not kept")
        let replacedGeneration = await coordinator.current.generation
        XCTAssertEqual(replacedGeneration, 2)

        // A healthy probe leaves the link alone.
        await coordinator.setProbe { true }
        await coordinator.retryImmediately()
        await settle()
        XCTAssertEqual(connector.attempts, 2)
        await coordinator.stop()
    }

    func testDiagnosticsOptionsReachTheConnector() async {
        let connector = ScriptedConnector([.connect(.relay)])
        let coordinator = RemoteLinkCoordinator(connector: connector)
        await coordinator.setOptions(RemoteLinkConnectOptions(skipLAN: true, peerWait: .seconds(7)))
        await coordinator.start(record: record())
        _ = await waitFor(coordinator) { $0 == .ready }
        XCTAssertEqual(connector.options.first, RemoteLinkConnectOptions(skipLAN: true, peerWait: .seconds(7)))
        await coordinator.stop()
    }
}
