import XCTest

@testable import LatchMobileKit

final class TranscriptTests: XCTestCase {
    private var epoch = 1
    private var clock = 0

    private func event(_ payload: HarnessEventPayload) -> HarnessEvent {
        clock += 1
        return HarnessEvent(
            sessionId: "ses_1",
            at: "2026-08-14T10:00:0\(clock)Z",
            harnessVersion: "1.0.0",
            connectorEpoch: epoch,
            payload: payload
        )
    }

    func testStreamingDeltasCollapseIntoOneAssistantRow() {
        var transcript = Transcript()
        transcript.apply(event(.assistantDelta(text: "Hel")))
        transcript.apply(event(.assistantDelta(text: "lo ")))
        transcript.apply(event(.assistantDelta(text: "there")))
        XCTAssertEqual(transcript.entries.count, 1)
        XCTAssertEqual(transcript.entries.first?.kind, .assistant("Hello there"))
    }

    func testTheCompleteMessageSupersedesItsDeltas() {
        // The harness sends both. Appending would double the turn, so the
        // final message replaces what the deltas built.
        var transcript = Transcript()
        transcript.apply(event(.assistantDelta(text: "Hel")))
        transcript.apply(event(.assistantDelta(text: "lo")))
        transcript.apply(event(.assistantMessage(text: "Hello there")))
        XCTAssertEqual(transcript.entries.count, 1)
        XCTAssertEqual(transcript.entries.first?.kind, .assistant("Hello there"))
    }

    func testANewTurnStartsAFreshAssistantRow() {
        var transcript = Transcript()
        transcript.apply(event(.assistantDelta(text: "one")))
        transcript.apply(event(.userMessage(text: "wait")))
        transcript.apply(event(.assistantDelta(text: "two")))
        XCTAssertEqual(
            transcript.entries.map(\.kind),
            [.assistant("one"), .user("wait"), .assistant("two")]
        )
    }

    func testAToolCallIsOneRowFromStartToFinish() {
        var transcript = Transcript()
        transcript.apply(
            event(.toolStarted(tool: "Bash", input: .object(["command": .string("ls -la")])))
        )
        XCTAssertEqual(
            transcript.entries.first?.kind,
            .tool(name: "Bash", detail: "ls -la", finished: false)
        )
        transcript.apply(event(.toolFinished(tool: "Bash", output: .string("a\nb"))))
        XCTAssertEqual(transcript.entries.count, 1)
        XCTAssertEqual(
            transcript.entries.first?.kind,
            .tool(name: "Bash", detail: "a\nb", finished: true)
        )
    }

    func testAFinishWithNoMatchingStartStillAppears() {
        // A cursor can land mid-tool-call, so the finish arrives alone.
        var transcript = Transcript()
        transcript.apply(event(.toolFinished(tool: "Read", output: .string("done"))))
        XCTAssertEqual(
            transcript.entries.first?.kind,
            .tool(name: "Read", detail: "done", finished: true)
        )
    }

    func testAPendingRequestIsTrackedUntilTheHarnessMovesOn() {
        var transcript = Transcript()
        transcript.apply(
            event(.awaitingInput(
                requestId: "req-1",
                kind: .permission,
                prompt: "Run ls?",
                choices: ["yes", "no"]
            ))
        )
        XCTAssertEqual(transcript.pendingRequest?.requestId, "req-1")
        XCTAssertEqual(transcript.pendingRequest?.choices, ["yes", "no"])

        transcript.apply(event(.status(status: "running")))
        XCTAssertNil(
            transcript.pendingRequest,
            "the harness moved on, so it is no longer blocked on the request"
        )
    }

    func testAnyLaterEventEndsThePendingRequest() {
        // The prompt can be answered at the computer instead of on the phone.
        // Whatever the harness does next proves it is no longer waiting, so the
        // buttons must not linger until a turn boundary happens to arrive.
        for later in [
            HarnessEventPayload.assistantMessage(text: "done"),
            .toolStarted(tool: "Bash", input: .null),
            .userMessage(text: "answered at the desk")
        ] {
            var transcript = Transcript()
            transcript.apply(
                event(.awaitingInput(requestId: "r", kind: .permission, prompt: "?", choices: []))
            )
            XCTAssertNotNil(transcript.pendingRequest)
            transcript.apply(event(later))
            XCTAssertNil(
                transcript.pendingRequest,
                "\(later.wireType) should have ended the pending request"
            )
        }
    }

    func testASecondRequestReplacesTheFirst() {
        var transcript = Transcript()
        transcript.apply(
            event(.awaitingInput(requestId: "r1", kind: .permission, prompt: "one", choices: []))
        )
        transcript.apply(
            event(.awaitingInput(requestId: "r2", kind: .permission, prompt: "two", choices: []))
        )
        XCTAssertEqual(transcript.pendingRequest?.requestId, "r2")
        XCTAssertEqual(transcript.entries.count, 2, "both requests stay in the transcript")
    }

    func testAnsweringARequestRecordsTheChoiceAndClearsThePrompt() {
        var transcript = Transcript()
        transcript.apply(
            event(.awaitingInput(requestId: "r", kind: .question, prompt: "Which?", choices: []))
        )
        transcript.markAnswered(requestId: "r", choice: "b")
        XCTAssertNil(transcript.pendingRequest)
        guard case .request(let request) = transcript.entries.first?.kind else {
            return XCTFail("expected a request row")
        }
        XCTAssertEqual(request.answered, "b")
    }

    func testAnUnknownEventTypeIsShownRatherThanHidden() {
        var transcript = Transcript()
        transcript.apply(event(.unknown(type: "thinking")))
        XCTAssertEqual(transcript.entries.first?.kind, .unrecognized("thinking"))
    }

    func testResyncDropsTheTimelineWithoutReusingRowIdentity() {
        // SwiftUI keys rows by id. Reusing an id after a replay would animate
        // an unrelated new row into an old one's place.
        var transcript = Transcript()
        transcript.apply(event(.userMessage(text: "first")))
        transcript.apply(event(.assistantMessage(text: "second")))
        let usedIds = Set(transcript.entries.map(\.id))

        transcript.reset()
        XCTAssertTrue(transcript.entries.isEmpty)
        XCTAssertNil(transcript.status)

        transcript.apply(event(.userMessage(text: "replayed")))
        let newId = try? XCTUnwrap(transcript.entries.first?.id)
        XCTAssertFalse(usedIds.contains(newId ?? -1))
    }

    func testStatusIsKeptForTheHeader() {
        var transcript = Transcript()
        transcript.apply(event(.status(status: "awaiting_input")))
        XCTAssertEqual(transcript.status, "awaiting_input")
    }

    func testToolInputSummaryPrefersARecognizableField() {
        XCTAssertEqual(JSONValue.object(["command": .string("git status")]).summary, "git status")
        XCTAssertEqual(JSONValue.object(["file_path": .string("/a/b.swift")]).summary, "/a/b.swift")
        // With nothing recognizable, the key names still say what was passed.
        XCTAssertEqual(JSONValue.object(["zeta": .bool(true), "alpha": .null]).summary, "alpha, zeta")
        XCTAssertEqual(JSONValue.number(42).summary, "42")
    }
}
