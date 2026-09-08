import XCTest

@testable import LatchMobileKit

/// The USB cold-open harness relies on two properties: the app writes exactly
/// one `cold_open` record per process, and the launch arguments that arm it
/// are parsed the same way every time. Neither needs a network.
@MainActor
final class ColdOpenRecorderTests: XCTestCase {
    func testOneRecordOnFirstApplicationReadyMeasuredFromProcessStart() {
        let writer = MemoryDiagnosticsWriter()
        var now = Date(timeIntervalSince1970: 1_000)
        let recorder = ColdOpenRecorder(
            writer: writer, launchedAt: Date(timeIntervalSince1970: 998.5), enabled: true, clock: { now }
        )
        recorder.observe(skipLAN: true)
        recorder.observe(path: RemotePath.relay)
        recorder.observe(RemoteLinkState.connecting(attempt: 1))
        recorder.observe(LinkStageSample(stage: .linkReady, milliseconds: 900))
        now = Date(timeIntervalSince1970: 1_001.25)
        recorder.observe(LinkStageSample(stage: .applicationReady, milliseconds: 1_100))
        // A later sample or state must not produce a second record.
        recorder.observe(LinkStageSample(stage: .applicationReady, milliseconds: 5))
        recorder.observe(RemoteLinkState.macOffline(nextRetryAt: now))

        XCTAssertEqual(writer.attempts.count, 1)
        let attempt = try! XCTUnwrap(writer.attempts.first)
        XCTAssertEqual(attempt.kind, .coldOpen)
        XCTAssertTrue(attempt.succeeded)
        XCTAssertEqual(attempt.path, "relay")
        XCTAssertTrue(attempt.skippedLAN)
        XCTAssertEqual(attempt.stages.map(\.stage), [.linkReady, .applicationReady, .launch])
        XCTAssertEqual(attempt.milliseconds(for: .launch), 2_750)
        XCTAssertFalse(recorder.isArmed)
    }

    func testTerminalFailureStatesEndTheAttemptAsFailed() {
        for state in [
            RemoteLinkState.macOffline(nextRetryAt: Date()),
            .revoked("gone"),
            .pairingRequired("gone"),
            .backoff(attempt: 3, nextRetryAt: Date(), reason: "refused"),
        ] {
            let writer = MemoryDiagnosticsWriter()
            let recorder = ColdOpenRecorder(writer: writer, launchedAt: Date(), enabled: true)
            recorder.observe(RemoteLinkState.backoff(attempt: 1, nextRetryAt: Date(), reason: "refused"))
            XCTAssertTrue(recorder.isArmed, "an early retry is not yet a failed cold open")
            recorder.observe(state)
            XCTAssertEqual(writer.attempts.count, 1, "\(state)")
            XCTAssertFalse(writer.attempts[0].succeeded)
            XCTAssertEqual(writer.attempts[0].failureStage, .linkReady)
            XCTAssertEqual(writer.attempts[0].stages.last?.stage, .launch)
            XCTAssertEqual(writer.attempts[0].stages.last?.outcome, .failed)
        }
    }

    func testAnOrdinaryLaunchWritesNothing() {
        let writer = MemoryDiagnosticsWriter()
        let recorder = ColdOpenRecorder(writer: writer, launchedAt: Date(), enabled: false)
        recorder.observe(LinkStageSample(stage: .applicationReady, milliseconds: 10))
        recorder.observe(RemoteLinkState.revoked("gone"))
        XCTAssertTrue(writer.attempts.isEmpty)
        XCTAssertNil(recorder.recorded)
    }

    func testLaunchArgumentsAreParsedAndBounded() {
        let none = DiagnosticsLaunchOptions(arguments: ["/app", "-AppleLanguages", "(en)"])
        XCTAssertEqual(none, DiagnosticsLaunchOptions())
        XCTAssertFalse(none.autoRuns)

        let options = DiagnosticsLaunchOptions(arguments: [
            "/app", "-latchDiagnosticsColdOpen", "1", "-latchDiagnosticsCycles", "9000",
            "-latchDiagnosticsSkipLAN", "true", "-latchDiagnosticsTerminal", "0",
            "-latchDiagnosticsPause", "-4", "-latchDiagnosticsCycles",
        ])
        XCTAssertTrue(options.recordColdOpen)
        XCTAssertEqual(options.autoRunCycles, 500)
        XCTAssertEqual(options.skipLAN, true)
        XCTAssertEqual(options.includeTerminal, false)
        XCTAssertEqual(options.pauseSeconds, 0)
        XCTAssertTrue(options.autoRuns)

        let saved = DiagnosticsSettings(skipLANAttempt: false, cycles: 30, includeTerminal: true, pauseSeconds: 2)
        let applied = options.applied(to: saved)
        XCTAssertEqual(applied, DiagnosticsSettings(skipLANAttempt: true, cycles: 500, includeTerminal: false, pauseSeconds: 0))
        let partial = DiagnosticsLaunchOptions(arguments: ["-latchDiagnosticsCycles", "12"]).applied(to: saved)
        XCTAssertEqual(partial, DiagnosticsSettings(skipLANAttempt: false, cycles: 12, includeTerminal: true, pauseSeconds: 2))
    }

    func testAppendingWriterKeepsOneFileAcrossLaunches() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("cold-open-writer-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let attempt = DiagnosticsAttempt(
            attempt: 1, kind: .coldOpen, startedAt: Date(timeIntervalSince1970: 0), path: "local",
            skippedLAN: false, stages: [LinkStageSample(stage: .launch, milliseconds: 1_200)],
            succeeded: true, failureStage: nil
        )
        for _ in 0..<2 {
            let writer = AppendingFileDiagnosticsWriter(directory: directory, fileName: "cold-opens.jsonl")
            try writer.begin(runStartedAt: Date())
            try writer.write(attempt)
        }
        let text = try String(contentsOf: directory.appendingPathComponent("cold-opens.jsonl"), encoding: .utf8)
        let lines = text.split(separator: "\n")
        XCTAssertEqual(lines.count, 2)
        XCTAssertTrue(lines.allSatisfy { $0.contains("\"kind\":\"cold_open\"") })
        XCTAssertTrue(lines.allSatisfy { $0.contains("\"stage\":\"launch\"") })
    }

    func testTheAppModelForwardsSamplesAndStatesToTheRecorder() async {
        let writer = MemoryDiagnosticsWriter()
        let recorder = ColdOpenRecorder(writer: writer, launchedAt: Date(), enabled: true)
        let model = AppModel(
            pairedGatewayFactory: { _ in throw LatchError.transport("no gateway in this test") },
            coldOpen: recorder
        )
        await model.setDiagnosticsSkipLAN(true)
        XCTAssertTrue(recorder.isArmed)
        XCTAssertTrue(model.coldOpen === recorder)
    }
}
