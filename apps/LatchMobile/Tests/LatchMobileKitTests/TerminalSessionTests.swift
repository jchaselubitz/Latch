import XCTest

@testable import LatchMobileKit

@MainActor
final class TerminalSessionTests: XCTestCase {
    private enum Dropped: Error { case now }

    private final class FakeConnection: TerminalSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var pending: [Data]
        private var sentBytes: [Data] = []
        private var sentControl: [String] = []
        private var cancelled = false
        let code: Int?

        init(output: [Data] = [], closeCode: Int? = nil) {
            pending = output
            code = closeCode
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
            if code != nil { throw Dropped.now }
            while !wasCancelled {
                try await Task.sleep(for: .milliseconds(5))
            }
            throw Dropped.now
        }

        func send(_ bytes: Data) async throws { lock.withLock { sentBytes.append(bytes) } }
        func sendControl(_ text: String) async throws { lock.withLock { sentControl.append(text) } }
        func cancel() { lock.withLock { cancelled = true } }
    }

    private actor Sizes {
        var declared: [(cols: Int, rows: Int)] = []
        func record(cols: Int, rows: Int) { declared.append((cols, rows)) }
        var count: Int { declared.count }
        var first: (cols: Int, rows: Int)? { declared.first }
    }

    /// A connection that never yields, so the session stays `.attached` for
    /// the duration of a test.
    private func openConnection() -> FakeConnection {
        FakeConnection(output: [Data("ready".utf8)])
    }

    private func settle() async {
        try? await Task.sleep(for: .milliseconds(120))
    }

    func testAttachDeclaresTheRequestedSize() async {
        let sizes = Sizes()
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { cols, rows in
            await sizes.record(cols: cols, rows: rows)
            return connection
        }

        session.attach(cols: 100, rows: 30)
        await settle()

        let declared = await sizes.first
        XCTAssertEqual(declared?.cols, 100)
        XCTAssertEqual(declared?.rows, 30)
        XCTAssertEqual(session.cols, 100)
        XCTAssertEqual(session.rows, 30)
    }

    func testAttachReportsAttachedAndHoldingTheSurface() async {
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { _, _ in connection }

        session.attach(cols: 80, rows: 24)
        await settle()

        XCTAssertEqual(session.state, .attached)
        XCTAssertTrue(session.stoleSurface)
    }

    func testOutputArrivesOnTheStream() async {
        let connection = FakeConnection(output: [Data("frame".utf8)])
        let session = TerminalSession(sessionID: "ses_a") { _, _ in connection }
        let stream = session.output

        session.attach(cols: 80, rows: 24)
        var iterator = stream.makeAsyncIterator()
        let first = await iterator.next()

        XCTAssertEqual(first, Data("frame".utf8))
    }

    func testInputIsSentAsBinaryAndResizeAsControl() async {
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { _, _ in connection }

        session.attach(cols: 80, rows: 24)
        await settle()
        session.send([0x03])
        session.resize(cols: 100, rows: 30)
        await settle()

        XCTAssertEqual(connection.binary, [Data([0x03])])
        XCTAssertEqual(connection.control.count, 1)
    }

    func testResizeToTheSameGridSendsNothing() async {
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { _, _ in connection }

        session.attach(cols: 80, rows: 24)
        await settle()
        session.resize(cols: 80, rows: 24)
        await settle()

        XCTAssertTrue(connection.control.isEmpty)
    }

    func testStolenCloseLeavesTheSessionClosedAndNotHoldingTheSurface() async {
        let session = TerminalSession(sessionID: "ses_a") { _, _ in
            FakeConnection(output: [Data("x".utf8)], closeCode: 4409)
        }

        session.attach(cols: 80, rows: 24)
        await settle()

        XCTAssertEqual(session.state, .closed(.stolen))
        XCTAssertFalse(session.stoleSurface)
    }

    func testSessionExitedCloseIsReportedAsItsOwnReason() async {
        let session = TerminalSession(sessionID: "ses_a") { _, _ in
            FakeConnection(output: [Data("x".utf8)], closeCode: 4410)
        }

        session.attach(cols: 80, rows: 24)
        await settle()

        XCTAssertEqual(session.state, .closed(.sessionExited))
    }

    func testDetachReleasesTheSurfaceAndCancelsTheConnection() async {
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { _, _ in connection }

        session.attach(cols: 80, rows: 24)
        await settle()
        session.detach()
        await settle()

        XCTAssertEqual(session.state, .closed(.detached))
        XCTAssertFalse(session.stoleSurface)
        XCTAssertTrue(connection.wasCancelled)
    }

    /// A closed terminal must not take the surface back on its own; only a
    /// deliberate `attach` reopens one.
    func testAClosedSessionDoesNotReattachOnItsOwn() async {
        let opens = Sizes()
        let session = TerminalSession(sessionID: "ses_a") { cols, rows in
            await opens.record(cols: cols, rows: rows)
            return FakeConnection(output: [Data("x".utf8)], closeCode: 4409)
        }

        session.attach(cols: 80, rows: 24)
        try? await Task.sleep(for: .milliseconds(900))

        let count = await opens.count
        XCTAssertEqual(count, 1)
        XCTAssertEqual(session.state, .closed(.stolen))
    }

    func testAttachWhileAttachedDoesNotOpenASecondConnection() async {
        let opens = Sizes()
        let connection = openConnection()
        let session = TerminalSession(sessionID: "ses_a") { cols, rows in
            await opens.record(cols: cols, rows: rows)
            return connection
        }

        session.attach(cols: 80, rows: 24)
        await settle()
        session.attach(cols: 80, rows: 24)
        await settle()

        let count = await opens.count
        XCTAssertEqual(count, 1)
    }
}
