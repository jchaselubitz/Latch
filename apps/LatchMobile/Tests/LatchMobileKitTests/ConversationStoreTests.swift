import XCTest

@testable import LatchMobileKit

@MainActor
final class ConversationStoreTests: XCTestCase {
    private final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
        var caches: [String: ConversationStoreCache] = [:]

        func load(sessionID: String) throws -> ConversationStoreCache? { caches[sessionID] }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws { caches[sessionID] = cache }
    }

    /// Records whole-cache saves and journal appends separately, the way the
    /// file storage distinguishes a rewrite from an incremental entry.
    private final class JournalingStorage: ConversationStoreStorage, @unchecked Sendable {
        var base: ConversationStoreCache?
        var entries: [ConversationJournalEntry] = []
        var saves = 0
        var reportedJournalBytes = 0

        func load(sessionID: String) throws -> ConversationStoreCache? {
            guard base != nil || !entries.isEmpty else { return nil }
            return (base ?? ConversationStoreCache()).applying(entries)
        }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws {
            base = cache
            entries = []
            saves += 1
        }
        func append(_ entry: ConversationJournalEntry, sessionID: String) throws -> Int {
            entries.append(entry)
            return reportedJournalBytes
        }
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

    func testThreeHistoryPagesRemainRetainedAndReachableBeyondRenderedWindow() throws {
        let storage = MemoryStorage()
        let store = ConversationStore(sessionID: "ses_long", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        func rows(_ range: ClosedRange<Int>) -> [ConversationItem] {
            range.map { message("m\($0)", ordinal: UInt64($0), text: "message \($0)") }
        }
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows(301...600), state: state(), hasMoreBefore: true, reason: "initial"
        ))))

        for (range, more) in [(201...300, true), (101...200, true), (1...100, false)] {
            let anchor = store.items.first?.id
            store.receive(.message(.historyPage(requestId: UUID().uuidString, items: rows(range), hasMoreBefore: more)))
            XCTAssertEqual(store.prependAnchor, anchor)
            XCTAssertTrue(store.items.contains(where: { $0.id == "m\(range.lowerBound)" }))
        }

        XCTAssertEqual(store.retainedItems.map(\.id), (1...600).map { "m\($0)" })
        XCTAssertEqual(storage.caches["ses_long"]?.items.map(\.id), store.retainedItems.map(\.id))
        let restored = ConversationStore(sessionID: "ses_long", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        XCTAssertEqual(restored.retainedItems.count, 600)
        XCTAssertEqual(restored.items.map(\.id), (301...600).map { "m\($0)" })
        XCTAssertEqual(store.items.count, 300)
        XCTAssertFalse(store.hasMoreBefore)
        XCTAssertTrue(store.hasNewerRendered)
        store.showNewer()
        XCTAssertEqual(store.items.last?.id, "m400")
        store.showNewer()
        store.showNewer()
        XCTAssertEqual(store.items.last?.id, "m600")
        XCTAssertTrue(store.hasEarlierRendered)
        store.loadOlder()
        XCTAssertEqual(store.items.first?.id, "m201")
        XCTAssertEqual(store.retainedItems.count, 600)
    }

    func testRetentionCapacityStopsRemotePagingBeforeAWholePageWouldBeDropped() throws {
        let store = ConversationStore(
            sessionID: "ses_capacity", gateway: try gateway(), operationRetentionSeconds: 60,
            storage: MemoryStorage(), maximumItems: 200
        )
        let rows = (101...200).map { message("m\($0)", ordinal: UInt64($0), text: "item") }
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows, state: state(), hasMoreBefore: true, reason: "initial"
        ))))
        XCTAssertTrue(store.hasMoreBefore)
        let older = (1...100).map { message("m\($0)", ordinal: UInt64($0), text: "item") }
        store.receive(.message(.historyPage(requestId: "page", items: older, hasMoreBefore: true)))
        XCTAssertEqual(store.retainedItems.count, 200)
        XCTAssertEqual(store.retainedItems.first?.id, "m1")
        XCTAssertFalse(store.hasMoreBefore)
        XCTAssertTrue(store.isHistoryLimitReached)
    }

    private func rows(_ range: ClosedRange<Int>) -> [ConversationItem] {
        range.map { message("m\($0)", ordinal: UInt64($0), text: "message \($0)") }
    }

    func testAppendingOneTailItemDoesNotReencodeOrRepersistTheTranscript() throws {
        let storage = JournalingStorage()
        let store = ConversationStore(sessionID: "ses_tail", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows(1...1_000), state: state(), hasMoreBefore: false, reason: "initial"
        ))))
        XCTAssertEqual(storage.saves, 1, "a snapshot replaces the transcript and is written whole")
        let measured = store.measuredItemCount

        store.receive(.message(.itemsUpserted(generation: "g", revision: 2, items: [message("tail", ordinal: 1_001, text: "par")])))
        store.publishPendingChanges()
        store.flushPersistence()

        XCTAssertEqual(store.items.last?.id, "tail")
        XCTAssertEqual(store.measuredItemCount - measured, 1, "only the changed item is measured for the byte budget")
        XCTAssertEqual(storage.saves, 1, "a tail append must not rewrite the cache")
        XCTAssertEqual(storage.entries.count, 1)
        XCTAssertEqual(storage.entries.first?.upserts.map(\.id), ["tail"])
        XCTAssertEqual(storage.entries.first?.revision, 2)
        XCTAssertEqual(storage.entries.first?.removedIDs, [])

        // Further partial updates of the same row coalesce into one entry.
        for (offset, text) in ["part", "partial", "partial text"].enumerated() {
            store.receive(.message(.itemsUpserted(
                generation: "g", revision: UInt64(3 + offset), items: [message("tail", ordinal: 1_001, text: text)]
            )))
        }
        store.publishPendingChanges()
        store.flushPersistence()
        XCTAssertEqual(store.measuredItemCount - measured, 4)
        XCTAssertEqual(storage.saves, 1)
        XCTAssertEqual(storage.entries.count, 2)
        XCTAssertEqual(storage.entries.last?.upserts, [message("tail", ordinal: 1_001, text: "partial text")])
        XCTAssertEqual(storage.entries.last?.revision, 5)

        let restored = ConversationStore(sessionID: "ses_tail", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        XCTAssertEqual(restored.retainedItems, store.retainedItems)
        XCTAssertEqual(restored.revision, 5)
    }

    func testStreamingChangesAreJournaledOnceAfterTheThrottleInterval() async throws {
        let storage = JournalingStorage()
        let store = ConversationStore(
            sessionID: "ses_throttle", gateway: try gateway(), operationRetentionSeconds: 60,
            storage: storage, persistInterval: .milliseconds(40)
        )
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows(1...3), state: state(), hasMoreBefore: false, reason: "initial"
        ))))
        store.receive(.message(.itemsUpserted(generation: "g", revision: 2, items: [message("m4", ordinal: 4, text: "a")])))
        store.receive(.message(.itemsRemoved(generation: "g", revision: 3, itemIds: ["m1"])))
        store.receive(.message(.stateChanged(generation: "g", revision: 3, state: state(canSend: false))))
        XCTAssertTrue(storage.entries.isEmpty, "streaming changes wait for the throttle")

        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(storage.entries.count, 1)
        XCTAssertEqual(storage.entries.first?.upserts.map(\.id), ["m4"])
        XCTAssertEqual(storage.entries.first?.removedIDs, ["m1"])
        XCTAssertEqual(storage.entries.first?.state, state(canSend: false))
        XCTAssertEqual(storage.saves, 1)

        store.receive(.message(.itemsUpserted(generation: "g", revision: 4, items: [message("m5", ordinal: 5, text: "b")])))
        store.stop()
        XCTAssertEqual(storage.entries.count, 2, "stopping before suspension writes what is pending")
        XCTAssertEqual(
            ConversationStore(sessionID: "ses_throttle", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
                .retainedItems.map(\.id),
            ["m2", "m3", "m4", "m5"]
        )
    }

    func testJournalCompactsOnceItOutgrowsTheTranscript() throws {
        let storage = JournalingStorage()
        let store = ConversationStore(sessionID: "ses_compact", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows(1...2), state: state(), hasMoreBefore: false, reason: "initial"
        ))))
        storage.reportedJournalBytes = 1024 * 1024
        store.receive(.message(.itemsUpserted(generation: "g", revision: 2, items: [message("m3", ordinal: 3, text: "c")])))
        store.flushPersistence()

        XCTAssertEqual(storage.saves, 2)
        XCTAssertTrue(storage.entries.isEmpty)
        XCTAssertEqual(storage.base?.items.map(\.id), ["m1", "m2", "m3"])
        XCTAssertEqual(storage.base?.journalSequence, 1, "the compacted base records the entries it covers")
    }

    func testFileStorageReplaysItsJournalAcrossRelaunch() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let storage = FileConversationStoreStorage(directory: directory)
        let store = ConversationStore(sessionID: "ses/file", gateway: try gateway(), operationRetentionSeconds: 60, storage: storage)
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g", revision: 1, operationEpoch: "e",
            items: rows(101...200), state: state(), hasMoreBefore: true, reason: "initial"
        ))))
        store.receive(.message(.historyPage(requestId: "h", items: rows(1...100), hasMoreBefore: false)))
        store.receive(.message(.itemsUpserted(generation: "g", revision: 2, items: [message("tail", ordinal: 201, text: "streamed")])))
        store.receive(.message(.itemsRemoved(generation: "g", revision: 3, itemIds: ["m150"])))
        store.stop()

        let journal = directory.appendingPathComponent("ses_file.journal")
        XCTAssertEqual(try String(contentsOf: journal, encoding: .utf8).split(separator: "\n").count, 2)

        let relaunched = ConversationStore(sessionID: "ses/file", gateway: try gateway(), operationRetentionSeconds: 60, storage: FileConversationStoreStorage(directory: directory))
        XCTAssertEqual(relaunched.retainedItems, store.retainedItems)
        XCTAssertEqual(relaunched.revision, 3)
        XCTAssertEqual(relaunched.items.last?.id, "tail")
        XCTAssertFalse(relaunched.hasMoreBefore)

        // A record torn by an interrupted write ends replay at the last whole
        // entry rather than failing the restore.
        let handle = try FileHandle(forWritingTo: journal)
        try handle.seekToEnd()
        try handle.write(contentsOf: Data(#"{"sequence":3,"generation":"#.utf8))
        try handle.close()
        XCTAssertEqual(try storage.load(sessionID: "ses/file")?.revision, 3)
    }

    func testFileStorageIgnoresEntriesAlreadyCoveredByACompactedBase() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let storage = FileConversationStoreStorage(directory: directory)
        func entry(_ sequence: UInt64, _ item: ConversationItem) -> ConversationJournalEntry {
            ConversationJournalEntry(
                sequence: sequence, generation: "g", revision: sequence, operationEpoch: "e", state: nil,
                hasMoreBefore: false, operations: [], upserts: [item], removedIDs: []
            )
        }
        _ = try storage.append(entry(1, message("a", ordinal: 1, text: "old")), sessionID: "s")
        _ = try storage.append(entry(2, message("b", ordinal: 2, text: "b")), sessionID: "s")
        // Simulates an interruption between writing the new base and removing
        // the journal it superseded: the stale entries must not replay.
        let journal = try Data(contentsOf: directory.appendingPathComponent("s.journal"))
        try storage.save(ConversationStoreCache(
            generation: "g", revision: 2, operationEpoch: "e",
            items: [message("a", ordinal: 1, text: "new"), message("b", ordinal: 2, text: "b")], journalSequence: 2
        ), sessionID: "s")
        try journal.write(to: directory.appendingPathComponent("s.journal"))
        _ = try storage.append(entry(3, message("c", ordinal: 3, text: "c")), sessionID: "s")
        _ = try storage.append(entry(5, message("gap", ordinal: 5, text: "gap")), sessionID: "s")

        let loaded = try XCTUnwrap(storage.load(sessionID: "s"))
        XCTAssertEqual(loaded.items.map(\.id), ["a", "b", "c"], "replay stops at a sequence gap")
        XCTAssertEqual(loaded.items.first, message("a", ordinal: 1, text: "new"))
        XCTAssertEqual(loaded.revision, 3)
        XCTAssertEqual(loaded.journalSequence, 3)
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
