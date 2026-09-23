import XCTest

@testable import LatchMobileKit

/// Every screen state the chat view can present, derived from store facts.
final class ConversationViewStateTests: XCTestCase {
    func testConversationSupportWaitsForAStateFrameBeforeCallingItUnavailable() {
        XCTAssertEqual(ConversationSupport.derive(state: nil), .loading)
        XCTAssertEqual(
            ConversationSupport.derive(state: state("starting")),
            .availableOrStarting,
            "a recognized connector may still be binding; nil connector is not a refusal"
        )
        XCTAssertEqual(ConversationSupport.derive(state: state("unavailable")), .unavailable)
    }

    func testEveryStateIsReachable() {
        let cases: [(ConversationViewState, ConversationSocketState, String?, ConversationState?, Bool)] = [
            (.loading, .connecting, nil, nil, false),
            (.starting, .open, nil, state("starting"), false),
            (.empty, .open, nil, state("idle"), false),
            (.ready, .open, nil, state("idle"), true),
            (.working, .open, nil, state("working"), true),
            (.awaitingInput, .open, nil, state("awaiting_input", pending: "req-1"), true),
            (.interrupted, .open, nil, state("exited"), true),
            (.disconnected, .reconnecting(attempt: 2), "connection lost", state("working"), true),
            (.failed, .open, nil, state("unavailable"), true),
        ]
        XCTAssertEqual(Set(cases.map(\.0)), Set(ConversationViewState.allCases))
        for (expected, socket, error, state, hasItems) in cases {
            XCTAssertEqual(
                ConversationViewState.derive(socketState: socket, connectionError: error, state: state, hasItems: hasItems),
                expected
            )
        }
    }

    func testAPendingRequestIsAwaitingInputWhateverThePhase() {
        XCTAssertEqual(
            ConversationViewState.derive(socketState: .open, connectionError: nil, state: state("working", pending: "r"), hasItems: true),
            .awaitingInput
        )
    }

    func testCodexStartingStateIsNotMistakenForAnUnopenedSocket() {
        let starting = state("starting")
        XCTAssertEqual(
            ConversationViewState.derive(socketState: .open, connectionError: nil, state: starting, hasItems: false),
            .starting
        )
        XCTAssertEqual(ConversationViewState.starting.label, "Starting…")
        XCTAssertTrue(ConversationComposerPresentation.derive(
            viewState: .starting, state: starting, canSend: true, sendReason: nil, connectionError: nil
        ).canSend)
        let waiting = ConversationComposerPresentation.derive(
            viewState: .starting, state: starting, canSend: false,
            sendReason: "waiting for the agent's empty composer", connectionError: nil
        )
        XCTAssertFalse(waiting.canSend)
        XCTAssertEqual(waiting.notice?.title, "Agent starting")
    }

    func testCachedContentWithoutStateIsStillLoading() {
        XCTAssertEqual(
            ConversationViewState.derive(socketState: .connecting, connectionError: nil, state: nil, hasItems: true),
            .loading
        )
    }

    func testAnErrorBeforeAnyContentIsNotDisconnected() {
        XCTAssertEqual(
            ConversationViewState.derive(socketState: .reconnecting(attempt: 1), connectionError: "refused", state: nil, hasItems: false),
            .loading
        )
    }

    func testAStoppedSocketWithAnErrorFails() {
        XCTAssertEqual(
            ConversationViewState.derive(socketState: .stopped, connectionError: "revoked", state: state("idle"), hasItems: true),
            .failed
        )
    }

    func testLabelsNeverClaimThinkingOrCompletion() {
        for state in ConversationViewState.allCases {
            let label = state.label?.lowercased() ?? ""
            XCTAssertFalse(label.contains("thinking"), "\(state)")
            XCTAssertFalse(label.contains("complete") || label.contains("done"), "\(state)")
        }
    }

    func testWorkingStatusNamesTheNewestRunningTool() {
        let transcript = ConversationProjection.project(
            items: [
                ConversationItem(id: "old", ordinal: 1, createdAt: "", kind: .tool(name: "Read", summary: "", status: "running", parentMessageId: nil)),
                ConversationItem(id: "new", ordinal: 2, createdAt: "", kind: .tool(name: "Bash", summary: "", status: "running", parentMessageId: nil)),
            ],
            state: nil
        )

        XCTAssertEqual(ConversationViewState.working.statusLine(in: transcript), "Working · Running Bash")
        XCTAssertEqual(ConversationViewState.ready.statusLine(in: transcript), nil)
    }

    private func state(_ phase: String, pending: String? = nil) -> ConversationState {
        ConversationState(
            phase: phase,
            sendMessage: OperationAvailability(enabled: phase == "idle", reason: nil),
            resolveRequest: OperationAvailability(enabled: pending != nil, reason: nil),
            pendingRequest: pending,
            connector: nil
        )
    }
}
