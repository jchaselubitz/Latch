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

    private func linkedModel() async -> AppModel {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.sessions)
        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { LatchGateway(link: $0, session: StubProtocol.session()) },
            presentationStore: MemorySessionPresentationStore(),
            terminalSizeStore: MemoryTerminalSizeStore(),
            terminalConnector: { _, _, _ in FakeConnection() }
        )
        await model.link(address: "https://mac.local:8787", token: "token")
        return model
    }

    private func settle() async {
        for _ in 0..<40 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(30))
    }

    func testBackgroundingDetachesAndForegroundingDoesNotReattach() async throws {
        let model = await linkedModel()
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

    /// Back-navigation is the other release point, and it goes further than
    /// backgrounding: nothing displays the connection's state once the screen
    /// is gone, so it is forgotten rather than merely detached.
    func testLeavingTheScreenGivesTheTerminalBackAndForgetsTheConnection() async throws {
        let model = await linkedModel()
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
            terminalConnector: { _, _, _ in FakeConnection() }
        )
        await model.link(address: "https://mac.local:8787", token: "token")

        let session = try XCTUnwrap(model.sessions.first)
        XCTAssertFalse(model.surface.terminal)
        XCTAssertNil(model.terminalSession(for: session))
    }
}
