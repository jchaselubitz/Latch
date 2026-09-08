import XCTest

@testable import LatchMobileKit

/// Terminal ownership across transport loss, and the diagnostics runner's
/// content-free record. Both are rules about what the phone may do on its
/// own; neither needs a network.
@MainActor
final class RecoveryLifecycleTests: XCTestCase {
    private enum Dropped: Error { case transport }

    /// A connection that announces the surface with an `attached` control
    /// frame carrying a resume capability, then stays open until dropped.
    private final class AnnouncingConnection: TerminalSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var frames: [TerminalInbound]
        private var dropped = false
        private var cancelled = false
        private(set) var sent: [Data] = []
        let code: Int?

        init(capability: String?, closeCode: Int? = nil) {
            code = closeCode
            var frames: [TerminalInbound] = []
            if let capability {
                frames.append(.control(#"{"type":"attached","resumeCapability":"\#(capability)","resumeWindowSeconds":60}"#))
            }
            frames.append(.output(Data("ready".utf8)))
            self.frames = frames
        }

        var closeCode: Int? { code }
        var sentBytes: [Data] { lock.withLock { sent } }

        func receive() async throws -> Data { throw Dropped.transport }

        func receiveFrame() async throws -> TerminalInbound {
            let next: TerminalInbound? = lock.withLock { frames.isEmpty ? nil : frames.removeFirst() }
            if let next { return next }
            if code != nil { throw Dropped.transport }
            while !(lock.withLock { dropped || cancelled }) {
                try await Task.sleep(for: .milliseconds(5))
            }
            throw Dropped.transport
        }

        func send(_ bytes: Data) async throws { lock.withLock { sent.append(bytes) } }
        func sendControl(_ text: String) async throws {}
        func cancel() { lock.withLock { cancelled = true } }
        func drop() { lock.withLock { dropped = true } }
    }

    /// Waits for a condition that a background poll loop will produce; the
    /// fixed settle below is enough locally but not on a loaded CI runner.
    private func waitUntil(_ condition: @escaping @MainActor () -> Bool) async {
        for _ in 0..<200 {
            if condition() { return }
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    private func settle() async {
        for _ in 0..<40 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(40))
    }

    func testTransportLossBecomesInterruptedWithoutReplayAndResumesWithTheCapability() async {
        let connection = AnnouncingConnection(capability: String(repeating: "ab", count: 32))
        final class Recorder: @unchecked Sendable { var resumes: [String?] = [] }
        let recorder = Recorder()
        let terminal = TerminalSession(sessionID: "ses_a") { _, _, resume in
            recorder.resumes.append(resume)
            return connection
        }
        terminal.attach(cols: 80, rows: 24)
        await settle()
        XCTAssertEqual(terminal.state, .attached)
        terminal.send(ArraySlice("ls\n".utf8))
        await settle()
        XCTAssertEqual(connection.sentBytes, [Data("ls\n".utf8)])

        // The socket dies without a close frame: interrupted, resumable, and
        // the keystrokes typed at it are reported as possibly undelivered.
        connection.drop()
        await waitUntil { terminal.state == .interrupted(resumable: true) }
        XCTAssertEqual(terminal.state, .interrupted(resumable: true))
        XCTAssertTrue(terminal.inputMayBeUndelivered)
        XCTAssertTrue(terminal.canResume)
        XCTAssertFalse(terminal.holdsSurface)

        // Input while interrupted is dropped, never queued for the next attach.
        terminal.send(ArraySlice("rm -rf /\n".utf8))
        await settle()
        XCTAssertEqual(connection.sentBytes.count, 1, "nothing is sent on a lost surface")

        // Resume presents the exact capability; nothing typed is replayed.
        XCTAssertTrue(terminal.resume())
        await settle()
        XCTAssertEqual(recorder.resumes, [nil, String(repeating: "ab", count: 32)])
        XCTAssertEqual(connection.sentBytes.count, 1)
    }

    func testAnAttachWithoutACapabilityRequiresAnExplicitReconnect() async {
        let connection = AnnouncingConnection(capability: nil)
        let terminal = TerminalSession(sessionID: "ses_b") { _, _, _ in connection }
        terminal.attach(cols: 80, rows: 24)
        await settle()
        XCTAssertEqual(terminal.state, .attached)
        connection.drop()
        await waitUntil { terminal.state == .interrupted(resumable: false) }
        XCTAssertEqual(terminal.state, .interrupted(resumable: false))
        XCTAssertFalse(terminal.canResume)
        XCTAssertFalse(terminal.resume(), "no capability means no automatic reattach")
        XCTAssertFalse(terminal.inputMayBeUndelivered, "nothing was typed, so nothing is in doubt")
    }

    func testADeliberateDetachGivesUpTheCapabilityAndARefusalNeverSteals() async {
        let connection = AnnouncingConnection(capability: String(repeating: "cd", count: 32))
        let terminal = TerminalSession(sessionID: "ses_c") { _, _, _ in connection }
        terminal.attach(cols: 80, rows: 24)
        await settle()
        terminal.detach()
        XCTAssertEqual(terminal.state, .closed(.detached))
        XCTAssertFalse(terminal.canResume)
        XCTAssertFalse(terminal.resume())

        // A gateway refusal (4411) is a reasoned close, not an interruption:
        // the person must Take Control deliberately.
        let refused = AnnouncingConnection(capability: nil, closeCode: 4411)
        let second = TerminalSession(sessionID: "ses_d") { _, _, _ in refused }
        second.attach(cols: 80, rows: 24)
        await settle()
        XCTAssertEqual(second.state, .closed(.resumeRefused))
        XCTAssertFalse(second.canResume)
    }

    func testTheResumeWindowExpires() async {
        var now = Date(timeIntervalSince1970: 1_800_000_000)
        let clock: @Sendable () -> Date = { now }
        let connection = AnnouncingConnection(capability: String(repeating: "ef", count: 32))
        let terminal = TerminalSession(sessionID: "ses_e", now: clock) { _, _, _ in connection }
        terminal.attach(cols: 80, rows: 24)
        await settle()
        connection.drop()
        await settle()
        XCTAssertTrue(terminal.canResume)
        now = now.addingTimeInterval(61)
        XCTAssertFalse(terminal.canResume, "the gateway's 60-second window bounds automatic resume")
        XCTAssertFalse(terminal.resume())
    }

    // MARK: - Diagnostics runner

    @MainActor
    private final class FakeSubject: DiagnosticsSubject {
        var cycles = 0
        var failAt: Int?
        var skipLAN: [Bool] = []

        func diagnosticsRecoveryCycle(skipLAN: Bool) async throws -> (path: String?, stages: [LinkStageSample]) {
            cycles += 1
            self.skipLAN.append(skipLAN)
            if failAt == cycles { throw LatchError.transport("no link") }
            return ("relay", [
                LinkStageSample(stage: .linkReady, milliseconds: UInt64(100 * cycles)),
                LinkStageSample(stage: .discovery, milliseconds: 20),
                LinkStageSample(stage: .applicationReady, milliseconds: UInt64(100 * cycles + 20)),
            ])
        }

        func diagnosticsSessionList() async throws -> LinkStageSample {
            LinkStageSample(stage: .sessionList, milliseconds: 15)
        }

        func diagnosticsPreview() async throws -> LinkStageSample? { nil }
        func diagnosticsTerminalFirstOutput() async throws -> LinkStageSample? {
            LinkStageSample(stage: .terminalFirstOutput, milliseconds: 250)
        }
    }

    func testTheRunnerRecordsRealAttemptsWithContentFreeStages() async throws {
        let writer = MemoryDiagnosticsWriter()
        let store = MemoryDiagnosticsSettingsStore(DiagnosticsSettings(skipLANAttempt: true, cycles: 4, includeTerminal: false, pauseSeconds: 0))
        let runner = DiagnosticsRunner(store: store, writer: writer, pause: { _ in })
        let subject = FakeSubject()
        subject.failAt = 3
        runner.run(subject: subject)
        for _ in 0..<200 where runner.isRunning { try? await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(runner.isRunning)
        XCTAssertEqual(runner.attempts.count, 4)
        XCTAssertEqual(runner.successCount, 3)
        XCTAssertEqual(subject.skipLAN, [true, true, true, true])
        XCTAssertEqual(runner.attempts[2].failureStage, .linkReady)
        XCTAssertEqual(runner.attempts[0].path, "relay")
        XCTAssertTrue(runner.attempts.allSatisfy(\.skippedLAN))
        XCTAssertEqual(runner.attempts[3].stages.map(\.stage), [.linkReady, .discovery, .applicationReady, .sessionList, .preview])
        XCTAssertEqual(runner.attempts[3].stages.last?.outcome, .skipped)
        // p95 over the three successes (120, 220, 420) is the largest.
        XCTAssertEqual(runner.p95(.applicationReady), 420)
        XCTAssertEqual(writer.attempts.count, 4)

        // The written record names stages and numbers only.
        let encoder = JSONEncoder()
        let text = String(decoding: try encoder.encode(writer.attempts), as: UTF8.self)
        // Stage names are the fixed vocabulary; the record must carry no
        // identifier, path, prompt, or output field.
        for forbidden in ["sessionId", "sessionName", "prompt", "\"path\":\"/", "cwd", "\"output\"", "ses_", "title"] {
            XCTAssertFalse(text.contains(forbidden), "diagnostics leaked \(forbidden)")
        }
    }

    func testDiagnosticsSettingsAreBoundedAndDefaultToNotSkippingLAN() {
        let defaults = DiagnosticsSettings()
        XCTAssertFalse(defaults.skipLANAttempt)
        XCTAssertEqual(defaults.cycles, 30)
        XCTAssertEqual(DiagnosticsSettings(cycles: 0).cycles, 1)
        XCTAssertEqual(DiagnosticsSettings(cycles: 10_000).cycles, 500)
    }
}
