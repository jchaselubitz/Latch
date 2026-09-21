import XCTest

@testable import LatchMobileKit

/// The input surface: answering the pending request, delivery outcomes, and
/// the per-session draft. Store rules run against a store with no socket;
/// wording and availability are pure presentation and need no view.
@MainActor
final class ConversationInputTests: XCTestCase {
    private final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
        var caches: [String: ConversationStoreCache] = [:]

        func load(sessionID: String) throws -> ConversationStoreCache? { caches[sessionID] }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws { caches[sessionID] = cache }
    }

    private func gateway() throws -> LatchGateway {
        LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: ""))
    }

    private func state(
        phase: String = "idle",
        pendingRequest: String? = nil,
        canSend: Bool = true,
        sendReason: String? = nil
    ) -> ConversationState {
        ConversationState(
            phase: phase,
            sendMessage: OperationAvailability(enabled: canSend, reason: canSend ? nil : sendReason),
            resolveRequest: OperationAvailability(enabled: pendingRequest != nil, reason: pendingRequest == nil ? "no pending request" : nil),
            pendingRequest: pendingRequest,
            connector: ConnectorIdentity(id: "test", version: "1")
        )
    }

    private func request(_ id: String, ordinal: UInt64, requestId: String, status: String = "pending") -> ConversationItem {
        ConversationItem(
            id: id,
            ordinal: ordinal,
            createdAt: "2026-09-21T00:00:00Z",
            kind: .request(requestId: requestId, requestType: "permission", prompt: "Allow?", choices: ["Yes", "No"], status: status)
        )
    }

    private func store(items: [ConversationItem] = [], state: ConversationState) throws -> ConversationStore {
        let store = ConversationStore(sessionID: "ses_input", gateway: try gateway(), operationRetentionSeconds: 60, storage: MemoryStorage())
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e", items: items,
            state: state, hasMoreBefore: false, reason: "initial"
        ))))
        return store
    }

    // MARK: Answering a request

    func testAnAnswerTargetsTheExactPendingRequestAndIsOfferedOnce() throws {
        let store = try store(items: [request("r1", ordinal: 1, requestId: "req-1")], state: state(phase: "awaiting_input", pendingRequest: "req-1"))

        store.resolve(requestID: "req-other", choice: "Yes")
        XCTAssertTrue(store.resolveAttempts.isEmpty, "only the request the host names can be answered")

        store.resolve(requestID: "req-1", choice: "Yes")
        store.resolve(requestID: "req-1", choice: "No")
        XCTAssertEqual(store.resolveAttempts.count, 1, "a second tap while the first is in flight is ignored")
        let attempt = try XCTUnwrap(store.resolveAttempt(for: "req-1"))
        XCTAssertEqual(attempt.requestId, "req-1")
        XCTAssertEqual(attempt.choice, "Yes")
        XCTAssertEqual(attempt.status, .sending)

        store.receive(.message(.operationResult(operationId: attempt.id, status: "accepted", itemId: nil, reason: nil)))
        store.resolve(requestID: "req-1", choice: "No")
        XCTAssertEqual(store.resolveAttempt(for: "req-1")?.status, .accepted, "an applied answer is not offered again")

        let transcript = ConversationProjection.project(items: store.items, state: store.state, resolveAttempts: store.resolveAttempts)
        XCTAssertEqual(transcript.pendingRequest?.requestId, "req-1")
        XCTAssertEqual(transcript.pendingRequest?.answer?.allowsAnotherAnswer, false)
    }

    func testARefusedAnswerIsExplainedOnTheCardNotRaisedAsAnError() throws {
        let store = try store(items: [request("r1", ordinal: 1, requestId: "req-1")], state: state(phase: "awaiting_input", pendingRequest: "req-1"))
        store.resolve(requestID: "req-1", choice: "Yes")
        let first = try XCTUnwrap(store.resolveAttempt(for: "req-1"))

        store.receive(.message(.operationResult(
            operationId: first.id, status: "refused", itemId: nil,
            reason: "the requested choice is not identifiable on the current screen"
        )))

        XCTAssertNil(store.connectionError)
        let answer = try XCTUnwrap(
            ConversationProjection.project(items: store.items, state: store.state, resolveAttempts: store.resolveAttempts)
                .pendingRequest?.answer
        )
        XCTAssertEqual(answer.outcome, .refused("The requested choice is not identifiable on the current screen."))
        XCTAssertEqual(answer.title, "“Yes” was not applied")
        XCTAssertTrue(answer.allowsAnotherAnswer)

        // Choosing again is a new operation, never the old one replayed.
        store.resolve(requestID: "req-1", choice: "No")
        let second = try XCTUnwrap(store.resolveAttempt(for: "req-1"))
        XCTAssertNotEqual(second.id, first.id)
        XCTAssertEqual(second.choice, "No")
        XCTAssertEqual(store.resolveAttempts.count, 1)
    }

    func testARefusalStaysWithTheRequestAfterItSettlesButAnAcceptedAnswerDoesNot() throws {
        let store = try store(
            items: [request("r1", ordinal: 1, requestId: "req-1"), request("r2", ordinal: 2, requestId: "req-2")],
            state: state(phase: "awaiting_input", pendingRequest: "req-1")
        )
        store.resolve(requestID: "req-1", choice: "Yes")
        let refused = try XCTUnwrap(store.resolveAttempt(for: "req-1"))
        store.receive(.message(.operationResult(operationId: refused.id, status: "refused", itemId: nil, reason: "the requested prompt is no longer visible")))

        store.receive(.message(.stateChanged(generation: "g", revision: 1, state: state(phase: "awaiting_input", pendingRequest: "req-2"))))
        store.resolve(requestID: "req-2", choice: "No")
        let accepted = try XCTUnwrap(store.resolveAttempt(for: "req-2"))
        store.receive(.message(.operationResult(operationId: accepted.id, status: "accepted", itemId: nil, reason: nil)))
        store.receive(.message(.stateChanged(generation: "g", revision: 1, state: state())))

        XCTAssertEqual(store.resolveAttempts.map(\.requestId), ["req-1"])
        let rows = ConversationProjection.project(
            items: [request("r1", ordinal: 1, requestId: "req-1", status: "dismissed")],
            state: store.state,
            resolveAttempts: store.resolveAttempts
        ).turns[0].entries
        guard case .request(let settled) = rows.first else { return XCTFail("expected the request row") }
        XCTAssertFalse(settled.isAwaitingAnswer)
        XCTAssertEqual(settled.answer?.explainsSettledRequest, true)
    }

    func testUnknownAndAmbiguousAnswerOutcomesAreUncertain() throws {
        let store = try store(items: [request("r1", ordinal: 1, requestId: "req-1")], state: state(phase: "awaiting_input", pendingRequest: "req-1"))
        store.resolve(requestID: "req-1", choice: "Yes")
        let attempt = try XCTUnwrap(store.resolveAttempt(for: "req-1"))
        store.receive(.message(.operationResult(operationId: attempt.id, status: "unknown", itemId: nil, reason: nil)))

        XCTAssertEqual(store.resolveAttempt(for: "req-1")?.status, .ambiguous)
        let answer = ConversationRequestAnswerPresentation(attempt: try XCTUnwrap(store.resolveAttempt(for: "req-1")))
        XCTAssertEqual(answer.title, "“Yes” may have been applied")
        XCTAssertEqual(answer.detail, "The host has no record of this answer. Check the transcript before answering again.")
    }

    func testAnAnswerWithoutAConnectionIsNotSent() async throws {
        let store = try store(items: [request("r1", ordinal: 1, requestId: "req-1")], state: state(phase: "awaiting_input", pendingRequest: "req-1"))
        store.resolve(requestID: "req-1", choice: "Yes")
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertEqual(store.resolveAttempt(for: "req-1")?.status, .notSent)
        XCTAssertEqual(
            ConversationRequestAnswerPresentation(attempt: try XCTUnwrap(store.resolveAttempt(for: "req-1"))).allowsAnotherAnswer,
            true
        )
    }

    // MARK: Delivery outcomes

    func testARefusedSendLeavesTheTranscriptAndKeepsItsExactText() throws {
        let store = try store(state: state())
        store.send(text: "run the tests")
        let operation = try XCTUnwrap(store.operations.first)
        XCTAssertEqual(store.items.map(\.id), [operation.optimisticItemID])

        store.receive(.message(.operationResult(operationId: operation.id, status: "refused", itemId: nil, reason: "the agent composer is no longer empty")))

        XCTAssertTrue(store.items.isEmpty, "a refused message was never in the conversation")
        let row = try XCTUnwrap(ConversationOperationPresentation.rows(for: store.operations).first)
        XCTAssertEqual(row.kind, .refused)
        XCTAssertEqual(row.text, "run the tests")
        XCTAssertEqual(row.detail, "The agent composer is no longer empty.")
        XCTAssertEqual(row.actions, [.edit, .sendAgain, .dismiss])
        XCTAssertEqual(row.label(for: .sendAgain), "Send again")
    }

    func testAnAmbiguousSendStaysVisibleAndIsDistinctFromARefusal() throws {
        let store = try store(state: state())
        store.send(text: "deploy")
        let operation = try XCTUnwrap(store.operations.first)
        store.receive(.message(.operationResult(operationId: operation.id, status: "ambiguous", itemId: nil, reason: "connection lost")))

        let transcript = ConversationProjection.project(items: store.items, state: store.state, operations: store.operations)
        XCTAssertEqual(transcript.turns.first?.prompt?.deliveryCaption, "Delivery unknown")
        let row = try XCTUnwrap(ConversationOperationPresentation.rows(for: store.operations).first)
        XCTAssertEqual(row.kind, .uncertain)
        XCTAssertEqual(row.title, "May have been delivered")
        XCTAssertEqual(row.actions, [.sendAgain, .dismiss], "no edit: the text may already be in the conversation")
        XCTAssertEqual(row.label(for: .sendAgain), "Send as new message")
    }

    func testSendingAgainIsANewOperationAndReplacesTheSettledRecord() throws {
        let store = try store(state: state())
        store.send(text: "deploy")
        let original = try XCTUnwrap(store.operations.first)
        store.receive(.message(.operationResult(operationId: original.id, status: "ambiguous", itemId: nil, reason: nil)))

        store.retry(original.id)

        XCTAssertEqual(store.operations.count, 1)
        let replacement = try XCTUnwrap(store.operations.first)
        XCTAssertNotEqual(replacement.id, original.id)
        XCTAssertEqual(replacement.text, "deploy")
        XCTAssertEqual(replacement.status, .sending)
        XCTAssertEqual(store.items.map(\.id), [replacement.optimisticItemID])
    }

    func testNothingRetriesWhileSendingOrWhenTheHostCannotAccept() throws {
        let store = try store(state: state())
        store.send(text: "hello")
        let sending = try XCTUnwrap(store.operations.first)
        store.retry(sending.id)
        store.dismissOperation(sending.id)
        XCTAssertEqual(store.operations.map(\.id), [sending.id], "a sending operation is neither resent nor forgotten")

        store.receive(.message(.operationResult(operationId: sending.id, status: "refused", itemId: nil, reason: nil)))
        store.receive(.message(.stateChanged(generation: "g", revision: 1, state: state(phase: "working", canSend: false, sendReason: "agent is working"))))
        store.retry(sending.id)
        XCTAssertEqual(store.operations.map(\.id), [sending.id])
        XCTAssertEqual(store.operations.first?.status, .refused)
    }

    func testEditReturnsTheTextToTheDraftWithoutLosingWhatWasTyped() throws {
        let store = try store(state: state())
        store.send(text: "first try")
        let operation = try XCTUnwrap(store.operations.first)
        store.receive(.message(.operationResult(operationId: operation.id, status: "refused", itemId: nil, reason: nil)))

        store.draft = "and another thing"
        store.editOperation(operation.id)

        XCTAssertEqual(store.draft, "and another thing\n\nfirst try")
        XCTAssertTrue(store.operations.isEmpty)
    }

    // MARK: Draft

    func testTheDraftSurvivesAReconnect() throws {
        let store = try store(state: state())
        store.draft = "half a thought"

        store.reconnect(using: try gateway(), operationRetentionSeconds: 60)
        store.receive(.state(.reconnecting(attempt: 1)))
        store.receive(.state(.open))

        XCTAssertEqual(store.draft, "half a thought")
    }

    // MARK: Composer availability

    func testAgentStatesThatBlockSendingReadAsStates() {
        let working = ConversationComposerPresentation.derive(
            viewState: .working, state: state(phase: "working", canSend: false, sendReason: "agent is working"),
            canSend: false, sendReason: "agent is working", connectionError: nil
        )
        XCTAssertFalse(working.canSend)
        XCTAssertEqual(working.notice?.kind, .agentState)
        XCTAssertEqual(working.notice?.title, "The agent is working")

        let awaiting = ConversationComposerPresentation.derive(
            viewState: .awaitingInput,
            state: state(phase: "awaiting_input", pendingRequest: "req-1", canSend: false, sendReason: "resolve the pending request first"),
            canSend: false, sendReason: "resolve the pending request first", connectionError: nil
        )
        XCTAssertEqual(awaiting.notice?.kind, .agentState)
        XCTAssertEqual(awaiting.notice?.title, "Waiting for your answer")
    }

    func testConnectionLossDisablesSendingEvenWhenTheLastStateAllowedIt() {
        let offline = ConversationComposerPresentation.derive(
            viewState: .disconnected, state: state(), canSend: true, sendReason: nil,
            connectionError: "the secure connection closed"
        )
        XCTAssertFalse(offline.canSend)
        XCTAssertEqual(offline.notice?.kind, .connection)
        XCTAssertEqual(offline.notice?.detail, "The secure connection closed.")

        let connecting = ConversationComposerPresentation.derive(
            viewState: .loading, state: nil, canSend: false, sendReason: "Waiting for conversation state", connectionError: nil
        )
        XCTAssertEqual(connecting.notice?.kind, .connection)
        XCTAssertEqual(connecting.notice?.detail, "Waiting for conversation state.")
    }

    func testOtherHostRefusalsCarryTheHostReason() {
        let busy = ConversationComposerPresentation.derive(
            viewState: .ready, state: state(canSend: false, sendReason: "the agent composer is not empty"),
            canSend: false, sendReason: "the agent composer is not empty", connectionError: nil
        )
        XCTAssertEqual(busy.notice?.kind, .unavailable)
        XCTAssertEqual(busy.notice?.detail, "The agent composer is not empty.")

        let ready = ConversationComposerPresentation.derive(
            viewState: .ready, state: state(), canSend: true, sendReason: nil, connectionError: nil
        )
        XCTAssertEqual(ready, ConversationComposerPresentation(canSend: true, notice: nil))
    }

    func testManualReviewOffersEveryWayOutAndSendingOffersNone() {
        let review = ConversationOperation(id: "op", text: "x", operationEpoch: "e", status: .manualReview, reason: "epoch changed")
        XCTAssertEqual(ConversationOperationPresentation(operation: review)?.kind, .needsReview)
        XCTAssertEqual(ConversationOperationPresentation(operation: review)?.actions, [.edit, .sendAgain, .dismiss])
        XCTAssertNil(ConversationOperationPresentation(operation: ConversationOperation(id: "s", text: "x", operationEpoch: "e")))
    }
}
