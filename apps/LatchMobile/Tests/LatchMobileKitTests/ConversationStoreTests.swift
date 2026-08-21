import XCTest

@testable import LatchMobileKit

@MainActor
final class ConversationStoreTests: XCTestCase {
    private final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
        var caches: [String: ConversationStoreCache] = [:]

        func load(sessionID: String) throws -> ConversationStoreCache? { caches[sessionID] }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws { caches[sessionID] = cache }
    }

    private func gateway() throws -> LatchGateway {
        LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: ""))
    }

    private func state(pendingRequest: String? = nil, canSend: Bool = true) -> ConversationState {
        ConversationState(
            phase: "ready",
            sendMessage: OperationAvailability(enabled: canSend, reason: canSend ? nil : "busy"),
            resolveRequest: OperationAvailability(enabled: pendingRequest != nil, reason: nil),
            pendingRequest: pendingRequest,
            connector: ConnectorIdentity(id: "claude", version: "1")
        )
    }

    private func message(_ id: String, ordinal: UInt64, text: String) -> ConversationItem {
        ConversationItem(
            id: id,
            ordinal: ordinal,
            createdAt: "2026-08-20T00:00:00Z",
            kind: .message(role: "assistant", text: text, status: .complete)
        )
    }

    func testRestoresCacheBeforeConnectionAndSnapshotReplacesItAtomically() throws {
        let storage = MemoryStorage()
        storage.caches["ses_1"] = ConversationStoreCache(
            generation: "old",
            revision: 4,
            operationEpoch: "epoch-old",
            items: [message("stale", ordinal: 1, text: "stale")],
            state: state(pendingRequest: "request-old"),
            hasMoreBefore: true
        )
        let store = ConversationStore(sessionID: "ses_1", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)

        XCTAssertEqual(store.items.map(\.id), ["stale"])
        XCTAssertEqual(store.revision, 4)

        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "new",
            revision: 1,
            operationEpoch: "epoch-new",
            items: [message("fresh", ordinal: 9, text: "fresh")],
            state: state(),
            hasMoreBefore: false,
            reason: "generation"
        ))))

        XCTAssertEqual(store.items.map(\.id), ["fresh"])
        XCTAssertNil(store.pendingRequest)
        XCTAssertEqual(store.generation, "new")
        XCTAssertEqual(store.operationEpoch, "epoch-new")
        XCTAssertFalse(store.hasMoreBefore)
    }

    func testRevisionedMutationsApplyOnceAndHistoryPrependPreservesAnchor() async throws {
        let store = ConversationStore(sessionID: "ses_2", gateway: try gateway(), operationRetentionSeconds: 60, storage: MemoryStorage())
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g",
            revision: 2,
            operationEpoch: "e",
            items: [message("two", ordinal: 2, text: "two")],
            state: state(),
            hasMoreBefore: true,
            reason: "initial"
        ))))

        store.receive(.message(.itemsUpserted(generation: "g", revision: 3, items: [message("three", ordinal: 3, text: "three")])))
        store.receive(.message(.itemsUpserted(generation: "g", revision: 3, items: [message("ignored", ordinal: 4, text: "ignored")])))
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertEqual(store.items.map(\.id), ["two", "three"])

        store.receive(.message(.historyPage(requestId: "h", items: [message("one", ordinal: 1, text: "one")], hasMoreBefore: false)))
        XCTAssertEqual(store.items.map(\.id), ["one", "two", "three"])
        XCTAssertEqual(store.prependAnchor, "two")
    }

    func testStateCompanionAtTheSameRevisionAppliesAndRevisionGapsDoNotAdvance() async throws {
        let store = ConversationStore(sessionID: "ses_gap", gateway: try gateway(), operationRetentionSeconds: 60, storage: MemoryStorage())
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g",
            revision: 2,
            operationEpoch: "e",
            items: [message("two", ordinal: 2, text: "two")],
            state: state(),
            hasMoreBefore: false,
            reason: "initial"
        ))))

        let requestState = state(pendingRequest: "request-1", canSend: false)
        store.receive(.message(.itemsUpserted(generation: "g", revision: 3, items: [message("three", ordinal: 3, text: "three")])))
        store.receive(.message(.stateChanged(generation: "g", revision: 3, state: requestState)))
        XCTAssertEqual(store.revision, 3)
        XCTAssertEqual(store.state, requestState)

        let laterState = state(canSend: false)
        store.receive(.message(.stateChanged(generation: "g", revision: 7, state: laterState)))
        try await Task.sleep(for: .milliseconds(10))
        XCTAssertEqual(store.revision, 3, "a state-only overflow must not skip missing item revisions")
        XCTAssertEqual(store.state, laterState, "live availability can still update while resync is requested")
    }

    func testObservedUserMessageReconcilesTheMatchingOptimisticSubmission() async throws {
        let store = ConversationStore(sessionID: "ses_reconcile", gateway: try gateway(), operationRetentionSeconds: 60, storage: MemoryStorage())
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e", items: [],
            state: state(), hasMoreBefore: false, reason: "initial"
        ))))
        store.send(text: "continue")
        XCTAssertEqual(store.operations.count, 1)

        let observed = ConversationItem(
            id: "source-user-1", ordinal: 1, createdAt: "2026-08-20T00:00:00Z",
            kind: .message(role: "user", text: "continue", status: .observed)
        )
        store.receive(.message(.itemsUpserted(generation: "g", revision: 2, items: [observed])))
        try await Task.sleep(for: .milliseconds(30))

        XCTAssertTrue(store.operations.isEmpty)
        XCTAssertEqual(store.items.map(\.id), ["source-user-1"])
    }

    func testOperationEpochChangeRequiresManualReviewInsteadOfReplay() throws {
        let operation = ConversationOperation(id: "op", text: "continue", operationEpoch: "old", status: .sending)
        let storage = MemoryStorage()
        storage.caches["ses_3"] = ConversationStoreCache(
            generation: "g",
            revision: 1,
            operationEpoch: "old",
            operations: [operation]
        )
        let store = ConversationStore(sessionID: "ses_3", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g",
            revision: 2,
            operationEpoch: "new",
            items: [],
            state: state(),
            hasMoreBefore: false,
            reason: "operation_epoch"
        ))))

        XCTAssertEqual(store.operations.first?.status, .manualReview)
        XCTAssertEqual(store.operations.first?.operationEpoch, "old")
    }

    func testRefusedAndAmbiguousOperationsRemainDistinctAndRetainText() throws {
        let store = ConversationStore(sessionID: "ses_4", gateway: try gateway(), operationRetentionSeconds: 60, storage: MemoryStorage())
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g",
            revision: 1,
            operationEpoch: "e",
            items: [],
            state: state(),
            hasMoreBefore: false,
            reason: "initial"
        ))))
        store.send(text: "first")
        let firstID = try XCTUnwrap(store.operations.first?.id)
        store.receive(.message(.operationResult(operationId: firstID, status: "refused", itemId: nil, reason: "read-only")))
        XCTAssertEqual(store.operations.first?.status, .refused)
        XCTAssertEqual(store.operations.first?.text, "first")

        store.send(text: "second")
        let secondID = try XCTUnwrap(store.operations.last?.id)
        store.receive(.message(.operationResult(operationId: secondID, status: "ambiguous", itemId: nil, reason: "connection lost")))
        XCTAssertEqual(store.operations.last?.status, .ambiguous)
        XCTAssertEqual(store.operations.last?.text, "second")
    }
}
