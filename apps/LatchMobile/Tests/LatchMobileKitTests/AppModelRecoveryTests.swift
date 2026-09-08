import XCTest

@testable import LatchMobileKit

/// The app model on top of the one link owner: what happens to screens,
/// terminals, the loopback capability, and creation retries when the link is
/// lost, comes back, or is downgraded while it is away.
@MainActor
final class AppModelRecoveryTests: XCTestCase {
    private static let capabilities = """
    {"protocolVersion":2,"productVersion":"2.0.0",
     "capabilities":{"create":true,"openViewer":true,"localAttach":true,
      "cloudAttach":false,"selfUpdate":true,"extensions":[]},
     "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
      "browseDirectories":true,"createSession":true},
     "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
     "operationRetentionSeconds":600}
    """

    private static let sessions = """
    {"sessions":[{"id":"ses_a","name":"api","title":null,"state":"running",
      "cwd":"/tmp/api","command_label":"claude","created_at":"2026-08-24T09:00:00Z",
      "last_activity_at":null,"idle_ms":0,"connector":"claude"}]}
    """

    private final class HoldingConnection: TerminalSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var frames: [TerminalInbound]
        private var dropped = false
        private var cancelled = false
        var closeCode: Int? { nil }
        init(capability: String) {
            frames = [
                .control(#"{"type":"attached","resumeCapability":"\#(capability)","resumeWindowSeconds":60}"#),
                .output(Data("ready".utf8)),
            ]
        }
        func receive() async throws -> Data { throw URLError(.networkConnectionLost) }
        func receiveFrame() async throws -> TerminalInbound {
            let next: TerminalInbound? = lock.withLock { frames.isEmpty ? nil : frames.removeFirst() }
            if let next { return next }
            while !(lock.withLock { dropped || cancelled }) { try await Task.sleep(for: .milliseconds(5)) }
            throw URLError(.networkConnectionLost)
        }
        func send(_ bytes: Data) async throws {}
        func sendControl(_ text: String) async throws {}
        func cancel() { lock.withLock { cancelled = true } }
        func drop() { lock.withLock { dropped = true } }
    }

    private static let pairedAt = Date(timeIntervalSince1970: 1_800_000_000)

    private func record(permission: DevicePermission = .control) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone", name: "Phone", devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: "mac", publicKey: String(repeating: "22", count: 32), name: "Mac"),
            permission: permission, comparison: "0123 4567 89ab cdef", pairedAt: Self.pairedAt,
            controlPlane: URL(string: "https://control.example")!, accessToken: "token"
        )
    }

    private func settle() async {
        for _ in 0..<40 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(40))
    }

    private func waitForLinked(_ model: AppModel) async {
        for _ in 0..<200 {
            if case .linked = model.linkState { return }
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    private nonisolated static func stubGateway() -> LatchGateway {
        LatchGateway(link: try! GatewayLink(address: "https://mac.local:8787", token: "token"), session: StubProtocol.session())
    }

    func testLinkLossKeepsStaleSessionsAndReconnectionRediscoversOncePerGeneration() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        var capabilityRequests: [String] = []
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        XCTAssertTrue(model.linkState.isUsable)
        XCTAssertEqual(model.sessions.count, 1)
        capabilityRequests = StubProtocol.requests.filter { $0.path == "/v2/capabilities" }.map(\.path)
        XCTAssertEqual(capabilityRequests.count, 1, "one discovery per authenticated link")

        // The link dies: the list stays, marked stale, and the state says
        // exactly what the owner is doing.
        connector.latest?.drop()
        for _ in 0..<200 {
            if case .interrupted = model.linkState { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        guard case .interrupted(_, let cached) = model.linkState else { return XCTFail("expected interrupted, got \(model.linkState)") }
        XCTAssertNotNil(cached)
        XCTAssertTrue(model.sessionsStale)
        XCTAssertEqual(model.sessions.count, 1, "cached rows stay readable")
        XCTAssertFalse(model.surface.chat, "nothing is offered on a link that is not usable")

        // The owner reconnects on its own; discovery runs once more, for the
        // new generation, and the list is fresh again.
        await waitForLinked(model)
        await settle()
        XCTAssertTrue(model.linkState.isUsable)
        XCTAssertFalse(model.sessionsStale)
        capabilityRequests = StubProtocol.requests.filter { $0.path == "/v2/capabilities" }.map(\.path)
        XCTAssertEqual(capabilityRequests.count, 2)
        XCTAssertEqual(connector.attempts, 2)
        model.unlink()
    }

    func testSuspendRotatesTheLoopbackCapabilityAndTheOldOneIsDead() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        final class Transports: @unchecked Sendable { var made: [RemoteLinkGatewayTransport] = [] }
        let transports = Transports()
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { record, provider, recorder in
                // The real adapter, so the capability really rotates; the
                // gateway client is stubbed because no Mac is here.
                let transport = try await RemoteLinkGatewayTransport.start(
                    authenticatedProvider: provider, pairedDevice: record, recorder: recorder
                )
                transports.made.append(transport)
                // The stub URL protocol answers every host, so the adapter's
                // loopback link can stand in as the gateway address.
                return LatchGateway(transport: transport, session: StubProtocol.session())
            },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore()
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        XCTAssertEqual(transports.made.count, 1)
        let first = transports.made[0]

        model.suspendPairedTransport()
        await settle()
        XCTAssertTrue(first.isStopped, "backgrounding stops the adapter and discards its capability")
        guard case .interrupted(.suspended, _) = model.linkState else { return XCTFail("expected suspended, got \(model.linkState)") }
        XCTAssertEqual(connector.latest?.closedByOwner, true, "the native link is closed with it")

        await model.resumeAfterSuspension()
        await waitForLinked(model)
        XCTAssertEqual(transports.made.count, 2)
        let second = transports.made[1]
        XCTAssertNotEqual(first.capabilityForTesting, second.capabilityForTesting, "a fresh capability per foreground")
        XCTAssertNotEqual(first.gatewayLink.url, second.gatewayLink.url, "and a fresh ephemeral port")
        XCTAssertFalse(second.isStopped)
        model.unlink()
        second.stop()
    }

    func testAnImmediateResumeAfterSuspendReconnectsInsteadOfBeingUndoneByTheLateSuspension() async throws {
        // The diagnostics runner (and a fast background/foreground) suspends
        // and resumes in the same tick. In the field the detached suspension
        // ran after resume: resume only probed the old supervisor, the late
        // suspension then closed the fresh link and left the owner suspended,
        // and the phone sat for 20 seconds with a ready link on the Mac.
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay), .connect(.relay)])
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore()
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        let first = try XCTUnwrap(connector.latest)

        let started = Date()
        model.suspendPairedTransport()
        await model.resumeAfterSuspension()
        XCTAssertLessThan(Date().timeIntervalSince(started), 5, "resume must not wait out the settle bound")
        guard case .linked = model.linkState else { return XCTFail("expected linked, got \(model.linkState)") }
        XCTAssertTrue(first.closedByOwner, "the suspended link was closed, not left racing the new one")
        XCTAssertEqual(connector.connections.count, 2, "exactly one reconnect on the same owner")
        XCTAssertFalse(connector.connections[1].isClosed, "the fresh link is the live one")

        // And a second immediate cycle behaves the same way.
        model.suspendPairedTransport()
        await model.resumeAfterSuspension()
        guard case .linked = model.linkState else { return XCTFail("expected linked after the second cycle") }
        XCTAssertEqual(connector.connections.count, 3)
        model.unlink()
    }

    func testAHungRequestFromThePreviousLinkDoesNotBlockTheNextLinkFromBecomingUsable() async throws {
        // In the field the diagnostics cycle suspended while the first link's
        // session refresh was in flight; the request hung on the stopped
        // adapter and, because the owner's snapshots were applied one after
        // another and discovery ran inline, every later state transition
        // queued behind it. The Mac showed the new link ready within a
        // second while the phone reported nothing for 20 seconds.
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        StubProtocol.hang(path: "/v2/sessions", count: 1)
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore()
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        // The first refresh is now hanging. Cycle the link underneath it.
        try? await Task.sleep(for: .milliseconds(100))
        let started = Date()
        model.suspendPairedTransport()
        await model.resumeAfterSuspension()
        XCTAssertLessThan(Date().timeIntervalSince(started), 5, "the new link must not wait behind the old request")
        guard case .linked = model.linkState else { return XCTFail("expected linked, got \(model.linkState)") }
        XCTAssertEqual(connector.connections.count, 2)
        for _ in 0..<100 where model.sessions.isEmpty { try? await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(model.sessions.count, 1, "the second link's refresh answers")
        model.unlink()
    }

    func testPermissionDowngradeDuringRecoveryClosesTheTerminalBeforeAnythingIsFetched() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let connector = ScriptedConnector([.connect(.relay), .connect(.relay)])
        let connection = HoldingConnection(capability: String(repeating: "ab", count: 32))
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalConnector: { _, _, _ in connection },
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        _ = await model.unlockTerminal()
        let session = try XCTUnwrap(model.sessions.first)
        let terminal = try XCTUnwrap(model.terminalSession(for: session))
        terminal.attach(cols: 80, rows: 24)
        await settle()
        XCTAssertEqual(terminal.state, .attached)

        // Transport loss: the held surface becomes interrupted and resumable.
        connector.latest?.drop()
        connection.drop()
        for _ in 0..<200 {
            if case .interrupted = model.linkState { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        await settle()
        XCTAssertEqual(terminal.state, .interrupted(resumable: true))

        // While away, the Mac downgraded this phone. Applying the record
        // closes the terminal outright: the lesser grant no longer covers it,
        // and no resume may run on the recovered link.
        XCTAssertTrue(model.applyPairedDeviceRecord(record(permission: .interact)))
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(terminal.canResume)
        await waitForLinked(model)
        XCTAssertEqual(model.resumeInterruptedTerminals(), 0)
        XCTAssertFalse(model.surface.terminal)
        XCTAssertNil(model.terminalSession(for: session), "no new terminal under the lesser grant")
        model.unlink()
    }

    func testCreationRetriesReuseTheRequestIDAndNeverStartASecondShell() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        StubProtocol.stub(path: "/v2/directories", body: #"{"path":"/Users/jake","parent":null,"entries":[],"nextCursor":null}"#)
        // The first attempt loses its reply (a 502 from the local adapter is
        // what a dropped link looks like to the client); the retry succeeds.
        StubProtocol.stub(method: "POST", path: "/v2/sessions", status: 502,
                          body: #"{"error":"remote_link_unreachable","reason":"The secure connection closed before the request completed."}"#)
        let connector = ScriptedConnector([.connect(.relay)])
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        let opened = await model.newSessionFolderBrowser(mode: .create)
        let browser = try XCTUnwrap(opened)
        await browser.startSession()
        XCTAssertNotNil(browser.error)
        let pending = try XCTUnwrap(browser.pendingCreationRequestID, "a lost reply keeps its idempotency key")

        StubProtocol.stub(method: "POST", path: "/v2/sessions", status: 201,
                          body: #"{"protocolVersion":2,"session":{"id":"ses_new","name":"shell","state":"running","createdAt":"2026-09-08T00:00:00Z"}}"#)
        await browser.startSession()
        XCTAssertNil(browser.error)
        XCTAssertEqual(browser.createdSessionID, "ses_new")
        let posts = StubProtocol.requests.filter { $0.method == "POST" && $0.path == "/v2/sessions" }
        XCTAssertEqual(posts.count, 2)
        XCTAssertTrue(posts.allSatisfy { $0.body.contains(pending.uuidString) }, "the same id both times; the Mac reconciles")
        model.unlink()
    }

    func testRevocationDuringRecoveryStopsRetryAndClearsEverythingHeld() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let connector = ScriptedConnector([.connect(.relay), .fail(.revoked("This phone was unpaired on the Mac."))])
        let connection = HoldingConnection(capability: String(repeating: "cd", count: 32))
        let model = AppModel(
            linkConnector: connector,
            gatewayFactory: { _, _, _ in Self.stubGateway() },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalConnector: { _, _, _ in connection },
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        await model.connectPairedDevice(record())
        await waitForLinked(model)
        _ = await model.unlockTerminal()
        let session = try XCTUnwrap(model.sessions.first)
        let terminal = try XCTUnwrap(model.terminalSession(for: session))
        terminal.attach(cols: 80, rows: 24)
        await settle()

        connector.latest?.drop()
        connection.drop()
        for _ in 0..<300 {
            if case .revoked = model.linkState { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        guard case .revoked(let reason) = model.linkState else { return XCTFail("expected revoked, got \(model.linkState)") }
        XCTAssertEqual(reason, "This phone was unpaired on the Mac.")
        XCTAssertFalse(model.canRetryAutomatically)
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(model.isTerminalUnlocked, "the owner grace window ends with the pairing")
        await settle()
        XCTAssertEqual(connector.attempts, 2, "no further attempts after a terminal failure")
        model.unlink()
    }
}
