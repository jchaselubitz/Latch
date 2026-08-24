import XCTest

@testable import LatchMobileKit

final class TerminalSocketTests: XCTestCase {
    private enum Dropped: Error { case now }

    /// A connection that delivers a scripted sequence of output frames and
    /// then fails, reporting a close code the way `URLSessionWebSocketTask`
    /// does after the peer closes.
    private final class FakeConnection: TerminalSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var pending: [Data]
        private var sentBytes: [Data] = []
        private var sentControl: [String] = []
        private var cancelled = false
        let code: Int?
        /// Fails the receive loop with no close code at all, which is what a
        /// dropped transport looks like: the peer never sent a close frame.
        let failsWithoutClosing: Bool

        init(output: [Data] = [], closeCode: Int? = nil, failsWithoutClosing: Bool = false) {
            pending = output
            code = closeCode
            self.failsWithoutClosing = failsWithoutClosing
        }

        var closeCode: Int? { code }

        var binary: [Data] { lock.withLock { sentBytes } }
        var control: [String] { lock.withLock { sentControl } }
        var wasCancelled: Bool { lock.withLock { cancelled } }

        /// Delivers the scripted frames, then either fails with the scripted
        /// close code or stays open, the way a live attach does between
        /// repaints.
        func receive() async throws -> Data {
            let next: Data? = lock.withLock {
                pending.isEmpty ? nil : pending.removeFirst()
            }
            if let next { return next }
            if code != nil || failsWithoutClosing { throw Dropped.now }
            while !wasCancelled {
                try await Task.sleep(for: .milliseconds(5))
            }
            throw Dropped.now
        }

        func send(_ bytes: Data) async throws {
            lock.withLock { sentBytes.append(bytes) }
        }

        func sendControl(_ text: String) async throws {
            lock.withLock { sentControl.append(text) }
        }

        func cancel() {
            lock.withLock { cancelled = true }
        }
    }

    private actor Recorder {
        var events: [TerminalSocketEvent] = []
        var opens = 0
        func record(_ event: TerminalSocketEvent) { events.append(event) }
        func opened() { opens += 1 }

        var closeReasons: [TerminalCloseReason?] {
            events.compactMap { event in
                if case .closed(let reason, _) = event { return .some(reason) }
                return nil
            }
        }

        var output: [Data] {
            events.compactMap { event in
                if case .output(let data) = event { return data }
                return nil
            }
        }
    }

    private func drain() async {
        try? await Task.sleep(for: .milliseconds(120))
    }

    func testOutputFramesReachTheHandler() async {
        let connection = FakeConnection(output: [Data([0x1b, 0x5b, 0x41]), Data("hi".utf8)])
        let recorder = Recorder()
        let socket = TerminalSocket(
            makeConnection: { connection },
            eventHandler: { await recorder.record($0) }
        )
        await socket.start()
        await drain()
        await socket.stop()

        let output = await recorder.output
        XCTAssertEqual(output, [Data([0x1b, 0x5b, 0x41]), Data("hi".utf8)])
    }

    func testInputIsSentAsBinary() async throws {
        let connection = FakeConnection(output: [Data("x".utf8)])
        let socket = TerminalSocket(makeConnection: { connection }, eventHandler: { _ in })
        await socket.start()
        await drain()
        try await socket.send(Data([0x03]))
        await socket.stop()

        XCTAssertEqual(connection.binary, [Data([0x03])])
        XCTAssertTrue(connection.control.isEmpty)
    }

    func testResizeIsAControlFrameWithTheDeclaredShape() async throws {
        let connection = FakeConnection(output: [Data("x".utf8)])
        let socket = TerminalSocket(makeConnection: { connection }, eventHandler: { _ in })
        await socket.start()
        await drain()
        try await socket.resize(cols: 100, rows: 30)
        await socket.stop()

        XCTAssertTrue(connection.binary.isEmpty, "a resize must never be typed into the pane")
        let frame = try XCTUnwrap(connection.control.first)
        let decoded = try JSONDecoder().decode(
            TerminalResizeFrame.self,
            from: Data(frame.utf8)
        )
        XCTAssertEqual(decoded, TerminalResizeFrame(cols: 100, rows: 30))
        XCTAssertEqual(decoded.type, "resize")
    }

    func testStolenCloseCodeSurfacesAsStolen() async {
        let recorder = Recorder()
        let socket = TerminalSocket(
            makeConnection: { FakeConnection(output: [Data("x".utf8)], closeCode: 4409) },
            eventHandler: { await recorder.record($0) }
        )
        await socket.start()
        await drain()

        let reasons = await recorder.closeReasons
        XCTAssertEqual(reasons, [.stolen])
    }

    func testSessionExitedCloseCodeSurfacesAsSessionExited() async {
        let recorder = Recorder()
        let socket = TerminalSocket(
            makeConnection: { FakeConnection(output: [Data("x".utf8)], closeCode: 4410) },
            eventHandler: { await recorder.record($0) }
        )
        await socket.start()
        await drain()

        let reasons = await recorder.closeReasons
        XCTAssertEqual(reasons, [.sessionExited])
    }

    func testATransportFailureWithNoCloseCodeReportsDetailInsteadOfAReason() async {
        let recorder = Recorder()
        let socket = TerminalSocket(
            makeConnection: { FakeConnection(output: [], failsWithoutClosing: true) },
            eventHandler: { await recorder.record($0) }
        )
        await socket.start()
        await drain()

        let events = await recorder.events
        let closed = events.compactMap { event -> (TerminalCloseReason?, String?)? in
            if case .closed(let reason, let detail) = event { return (reason, detail) }
            return nil
        }
        XCTAssertEqual(closed.count, 1)
        XCTAssertNil(closed.first?.0)
        XCTAssertNotNil(closed.first?.1)
    }

    /// The one deliberate divergence from `ConversationSocket`. Reopening a
    /// terminal is another steal, so a closed socket stays closed.
    func testAClosedSocketDoesNotReconnectOnItsOwn() async {
        let recorder = Recorder()
        let socket = TerminalSocket(
            makeConnection: {
                await recorder.opened()
                return FakeConnection(output: [Data("x".utf8)], closeCode: 4409)
            },
            eventHandler: { await recorder.record($0) }
        )
        await socket.start()
        // Long enough for `ConversationSocket`'s first two backoff delays.
        try? await Task.sleep(for: .milliseconds(900))

        let opens = await recorder.opens
        XCTAssertEqual(opens, 1)
        let reasons = await recorder.closeReasons
        XCTAssertEqual(reasons, [.stolen], "one close, not a retry loop")
    }
}
