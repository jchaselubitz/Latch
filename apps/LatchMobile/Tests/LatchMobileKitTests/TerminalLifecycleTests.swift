import XCTest

@testable import LatchMobileKit

/// The release points, which are mandatory because while the phone holds the
/// socket the desk does not have the surface.
///
/// The connection is faked rather than dialled: these are rules about
/// `AppModel`, and asserting them should not require a WebSocket listener.
@MainActor
final class TerminalLifecycleTests: XCTestCase {
    private enum Dropped: Error { case now }

    /// A connection that stays open until it is cancelled, the way a live
    /// attach does between repaints.
    private final class FakeConnection: TerminalSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var cancelled = false
        private var delivered = false

        var closeCode: Int? { nil }
        var wasCancelled: Bool { lock.withLock { cancelled } }

        func receive() async throws -> Data {
            let first: Bool = lock.withLock {
                defer { delivered = true }
                return !delivered
            }
            if first { return Data("ready".utf8) }
            while !wasCancelled {
                try await Task.sleep(for: .milliseconds(5))
            }
            throw Dropped.now
        }

        func send(_ bytes: Data) async throws {}
        func sendControl(_ text: String) async throws {}
        func cancel() { lock.withLock { cancelled = true } }
    }

    private static let capabilities = """
    {"protocolVersion":2,"productVersion":"2.0.0",
     "capabilities":{"create":true,"openViewer":true,"localAttach":true,
      "cloudAttach":false,"selfUpdate":true,"extensions":[]},
     "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true},
     "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
     "operationRetentionSeconds":600}
    """

    private static let sessions = """
    {"sessions":[{"id":"ses_a","name":"api","title":null,"state":"running",
      "cwd":"/tmp/api","command_label":"claude","created_at":"2026-08-24T09:00:00Z",
      "last_activity_at":null,"idle_ms":0,"connector":"claude"}]}
    """

    private func linkedModel(
        authenticator: StubDeviceOwnerAuthenticator = StubDeviceOwnerAuthenticator()
    ) async -> AppModel {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { LatchGateway(link: $0, session: StubProtocol.session()) },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalConnector: { _, _, _ in FakeConnection() },
            terminalUnlock: TerminalUnlock(authenticator: authenticator, grace: 600)
        )
        await model.link(address: "https://mac.local:8787", token: "token")
        return model
    }

    /// The owner check the screen runs before it takes anything. Everything
    /// below is about what happens once the terminal is open.
    private func unlocked(_ model: AppModel) async {
        let opened = await model.unlockTerminal()
        XCTAssertTrue(opened, "the stub authenticator approves")
    }

    private func settle() async {
        for _ in 0..<40 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(30))
    }

    func testBackgroundingDetachesAndForegroundingDoesNotReattach() async throws {
        let model = await linkedModel()
        await unlocked(model)
        let session = try XCTUnwrap(model.sessions.first)
        let terminal = try XCTUnwrap(model.terminalSession(for: session))

        terminal.attach(cols: 100, rows: 30)
        await settle()
        XCTAssertEqual(terminal.state, .attached)
        XCTAssertTrue(terminal.stoleSurface)

        // A phone suspended with the socket open holds the session's only
        // surface hostage from a locked pocket.
        model.suspendTerminals()
        await settle()
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(terminal.stoleSurface)

        // Coming back is not permission to take it again. Reattaching is
        // another steal, and the user should watch it happen.
        await model.resumeAfterSuspension()
        await settle()
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(terminal.stoleSurface)
        XCTAssertTrue(
            model.terminalSession(for: session) === terminal,
            "the connection is kept so the screen can offer Reattach, not replaced"
        )
    }

    /// The other end of the same rule. Backgrounding proper releases at once;
    /// merely losing focus — the app switcher, a notification pull, the Face ID
    /// prompt in front of the terminal — holds the surface, but not forever.
    func testAnIdleTerminalIsReleasedAndTheOwnerCheckGoesWithIt() async throws {
        let model = await linkedModel()
        await unlocked(model)
        let session = try XCTUnwrap(model.sessions.first)
        let terminal = try XCTUnwrap(model.terminalSession(for: session))

        terminal.attach(cols: 100, rows: 30)
        await settle()
        XCTAssertEqual(terminal.state, .attached)
        XCTAssertTrue(model.isTerminalUnlocked)

        // Well inside the window, nothing is taken away.
        XCTAssertEqual(model.releaseIdleTerminals(timeout: 120, now: Date().addingTimeInterval(30)), 0)
        XCTAssertEqual(terminal.state, .attached)

        // Typing resets it: the clock is time since input, not time since
        // attach, so a person working in a terminal keeps it.
        terminal.send(ArraySlice("ls\n".utf8))
        await settle()
        XCTAssertEqual(model.releaseIdleTerminals(timeout: 120, now: Date().addingTimeInterval(60)), 0)

        // Past the window with nothing typed, the Mac gets its surface back.
        XCTAssertEqual(model.releaseIdleTerminals(timeout: 120, now: Date().addingTimeInterval(200)), 1)
        await settle()
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(terminal.stoleSurface)
        // And it takes the owner check with it: a terminal given up because
        // nobody was there is reopened deliberately, not by picking the phone
        // back up inside the grace window.
        XCTAssertFalse(model.isTerminalUnlocked)
        XCTAssertNil(model.terminalSession(for: session))
    }

    /// A terminal nobody is holding is not released, and an idle release with
    /// nothing to release must not end a grace window a live terminal is using.
    func testTheIdleReleaseLeavesADetachedTerminalAndItsGraceWindowAlone() async throws {
        let model = await linkedModel()
        await unlocked(model)
        let session = try XCTUnwrap(model.sessions.first)
        _ = try XCTUnwrap(model.terminalSession(for: session))

        XCTAssertEqual(model.releaseIdleTerminals(timeout: 120, now: Date().addingTimeInterval(600)), 0)
        XCTAssertTrue(model.isTerminalUnlocked)
    }

    /// Back-navigation is the other release point, and it goes further than
    /// backgrounding: nothing displays the connection's state once the screen
    /// is gone, so it is forgotten rather than merely detached.
    func testLeavingTheScreenGivesTheTerminalBackAndForgetsTheConnection() async throws {
        let model = await linkedModel()
        await unlocked(model)
        let session = try XCTUnwrap(model.sessions.first)
        let terminal = try XCTUnwrap(model.terminalSession(for: session))

        terminal.attach(cols: 100, rows: 30)
        await settle()
        XCTAssertEqual(terminal.state, .attached)

        model.discardTerminal(for: session)
        await settle()
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(
            model.terminalSession(for: session) === terminal,
            "a re-entered screen gets a fresh connection, and a fresh output stream with it"
        )
    }

    /// The gate is the grant, not the screen: a phone that may not open a
    /// terminal is refused here rather than at the socket.
    func testAPhoneWithoutTheTerminalSurfaceIsNeverGivenAConnection() async throws {
        StubProtocol.reset()
        StubProtocol.stub(
            path: "/v2/capabilities",
            body: Self.capabilities.replacingOccurrences(of: "\"terminal\":true", with: "\"terminal\":false")
        )
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { LatchGateway(link: $0, session: StubProtocol.session()) },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalConnector: { _, _, _ in FakeConnection() },
            terminalUnlock: TerminalUnlock(
                authenticator: StubDeviceOwnerAuthenticator(),
                grace: 600
            )
        )
        await model.link(address: "https://mac.local:8787", token: "token")

        let session = try XCTUnwrap(model.sessions.first)
        XCTAssertFalse(model.surface.terminal)
        XCTAssertNil(model.terminalSession(for: session))
        // The owner check is never even offered: there is nothing behind it
        // this device is allowed to open.
        let opened = await model.unlockTerminal()
        XCTAssertFalse(opened)
    }

    /// The Mac's grant says the phone *may* open a terminal. The owner check
    /// says whoever is holding the phone right now is its owner. Both are
    /// required, and the second one is not implied by the first.
    func testTheGrantAloneDoesNotOpenATerminalWithoutTheOwnerCheck() async throws {
        let authenticator = StubDeviceOwnerAuthenticator(approves: false)
        let model = await linkedModel(authenticator: authenticator)
        let session = try XCTUnwrap(model.sessions.first)

        XCTAssertTrue(model.surface.terminal, "the Mac granted the terminal")
        XCTAssertNil(
            model.terminalSession(for: session),
            "no connection before the owner has confirmed"
        )

        let refused = await model.unlockTerminal()
        XCTAssertFalse(refused)
        XCTAssertEqual(authenticator.prompts, 1)
        XCTAssertNil(model.terminalSession(for: session))
        XCTAssertFalse(model.isTerminalUnlocked)

        // Chat is untouched by any of this: it is what a paired phone is for.
        XCTAssertNotNil(model.conversationStore(for: session))

        authenticator.set(approves: true)
        let opened = await model.unlockTerminal()
        XCTAssertTrue(opened)
        XCTAssertTrue(model.isTerminalUnlocked)
        XCTAssertNotNil(model.terminalSession(for: session))
    }

    /// Unlinking ends the grace window, so a phone relinked to another Mac
    /// starts from a fresh check rather than inheriting one.
    func testTearingDownEveryTerminalEndsTheGraceWindow() async throws {
        let authenticator = StubDeviceOwnerAuthenticator()
        let model = await linkedModel(authenticator: authenticator)
        await unlocked(model)
        XCTAssertTrue(model.isTerminalUnlocked)

        model.detachAllTerminals()
        XCTAssertFalse(model.isTerminalUnlocked)

        await unlocked(model)
        XCTAssertEqual(authenticator.prompts, 2)
    }
}
