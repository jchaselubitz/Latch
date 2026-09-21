import XCTest

@testable import LatchMobileKit

/// Current chat behaviour, replayed from the captured Claude corpus.
///
/// Connector projection is checked in Rust against each case's `expected.json`.
/// These tests load that same wire snapshot into `ConversationStore` so a
/// regression in decode, publish, pending-request selection, or the 300-item
/// window fails here. `ConversationProjection`'s switch is exhaustive on
/// `ConversationItemKind`; this suite fails to compile if a new kind is added
/// without a presentable arm.
@MainActor
final class ConversationFixtureTests: XCTestCase {
    private final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
        var caches: [String: ConversationStoreCache] = [:]

        func load(sessionID: String) throws -> ConversationStoreCache? { caches[sessionID] }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws {
            caches[sessionID] = cache
        }
    }

    private struct ExpectedFile: Decodable {
        let snapshot: ConversationSnapshot
        let projectedItemCount: Int
        let truncateAfter: [String]
    }

    private struct OptimisticFile: Decodable {
        let baseCase: String
        let optimistic: Optimistic
        struct Optimistic: Decodable {
            let id: String
            let text: String
        }
    }

    private struct OperationsFile: Decodable {
        let baseCase: String
        let operations: [Record]
        struct Record: Decodable {
            let id: String
            let text: String
            let status: String
            let reason: String
        }
    }

    private func gateway() throws -> LatchGateway {
        LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: ""))
    }

    private func makeStore(
        sessionID: String,
        storage: MemoryStorage = MemoryStorage()
    ) throws -> ConversationStore {
        ConversationStore(
            sessionID: sessionID,
            gateway: try gateway(),
            operationRetentionSeconds: 60,
            storage: storage
        )
    }

    func testEveryClaudeCaseSnapshotPublishesInStoreOrder() throws {
        let cases = try Self.caseDirectories()
        XCTAssertFalse(cases.isEmpty, "expected captured Claude cases")
        var sawMessage = false
        var sawTool = false
        var sawRequest = false
        for directory in cases {
            let expected = try Self.loadExpected(in: directory)
            let store = try makeStore(sessionID: directory.lastPathComponent)
            store.receive(.message(.snapshot(expected.snapshot)))
            XCTAssertEqual(
                store.items.map(\.id),
                expected.snapshot.items.map(\.id),
                directory.lastPathComponent
            )
            XCTAssertEqual(
                store.items.map(\.ordinal),
                expected.snapshot.items.map(\.ordinal),
                directory.lastPathComponent
            )
            XCTAssertEqual(store.state, expected.snapshot.state, directory.lastPathComponent)
            XCTAssertEqual(store.state?.pendingRequest, expected.snapshot.state.pendingRequest)
            if let pendingID = expected.snapshot.state.pendingRequest {
                guard case .request(let requestID, _, _, _, _) = store.pendingRequest?.kind else {
                    XCTFail("\(directory.lastPathComponent) pending request missing")
                    continue
                }
                XCTAssertEqual(requestID, pendingID, directory.lastPathComponent)
            }
            for item in store.items {
                assertPresentable(item, caseName: directory.lastPathComponent)
                switch item.kind {
                case .message: sawMessage = true
                case .tool: sawTool = true
                case .request: sawRequest = true
                case .unrecognized: XCTFail("\(directory.lastPathComponent) projected an unrecognized item")
                }
            }
        }
        XCTAssertTrue(sawMessage && sawTool && sawRequest, "the corpus must exercise every current row kind")
    }

    func testLongTranscriptIsSizedToTheStorePublishedWindow() throws {
        let expected = try Self.loadExpected(in: Self.claudeCases.appendingPathComponent("long-transcript"))
        XCTAssertGreaterThan(expected.projectedItemCount, 300)
        XCTAssertLessThanOrEqual(expected.snapshot.items.count, 300)
        XCTAssertTrue(expected.snapshot.hasMoreBefore)
        let store = try makeStore(sessionID: "long")
        store.receive(.message(.snapshot(expected.snapshot)))
        XCTAssertEqual(store.items.count, expected.snapshot.items.count)
        XCTAssertEqual(store.items.last?.id, expected.snapshot.items.last?.id)
        XCTAssertTrue(store.hasMoreBefore)
    }

    func testReconnectKeepsAnOptimisticRowUntilCanonicalObservation() async throws {
        let fixture: OptimisticFile = try Self.loadJSON(
            Self.presentation.appendingPathComponent("reconnect-optimistic.json")
        )
        let expected = try Self.loadExpected(in: Self.claudeCases.appendingPathComponent(fixture.baseCase))
        let storage = MemoryStorage()
        let operation = ConversationOperation(
            id: fixture.optimistic.id,
            text: fixture.optimistic.text,
            operationEpoch: expected.snapshot.operationEpoch,
            status: .sending
        )
        storage.caches["ses_reconnect"] = ConversationStoreCache(
            generation: expected.snapshot.generation,
            revision: expected.snapshot.revision,
            operationEpoch: expected.snapshot.operationEpoch,
            items: expected.snapshot.items,
            state: expected.snapshot.state,
            hasMoreBefore: expected.snapshot.hasMoreBefore,
            operations: [operation]
        )
        let store = try makeStore(sessionID: "ses_reconnect", storage: storage)
        store.receive(.message(.snapshot(expected.snapshot)))

        let optimisticID = "operation:\(fixture.optimistic.id)"
        XCTAssertEqual(store.items.last?.id, optimisticID)
        XCTAssertEqual(store.operations.count, 1)
        XCTAssertEqual(store.operations.first?.status, .sending)
        if case .message(let role, let text, let status) = store.items.last?.kind {
            XCTAssertEqual(role, "user")
            XCTAssertEqual(text, fixture.optimistic.text)
            XCTAssertEqual(status, .submitted)
        } else {
            XCTFail("optimistic reconnect row must render as a submitted user message")
        }

        let observed = ConversationItem(
            id: "source-user-paired",
            ordinal: (store.items.map(\.ordinal).filter { $0 != UInt64.max }.max() ?? 0) + 1,
            createdAt: "2026-09-08T07:26:00Z",
            kind: .message(role: "user", text: fixture.optimistic.text, status: .observed)
        )
        store.receive(
            .message(
                .itemsUpserted(
                    generation: expected.snapshot.generation,
                    revision: expected.snapshot.revision + 1,
                    items: [observed]
                )
            )
        )
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertFalse(store.items.contains { $0.id == optimisticID })
        XCTAssertTrue(store.items.contains { $0.id == observed.id })
        XCTAssertTrue(store.operations.isEmpty)
    }

    func testRefusedAndAmbiguousOperationsStayDistinctOnACapturedTranscript() throws {
        let fixture: OperationsFile = try Self.loadJSON(
            Self.presentation.appendingPathComponent("operations-refused-ambiguous.json")
        )
        let expected = try Self.loadExpected(in: Self.claudeCases.appendingPathComponent(fixture.baseCase))
        let store = try makeStore(sessionID: "ses_ops")
        store.receive(.message(.snapshot(expected.snapshot)))
        for record in fixture.operations {
            store.receive(
                .message(
                    .operationResult(
                        operationId: record.id,
                        status: record.status,
                        itemId: nil,
                        reason: record.reason
                    )
                )
            )
        }
        XCTAssertTrue(store.operations.isEmpty, "receipts for unknown ids must not invent operations")

        let storage = MemoryStorage()
        storage.caches["ses_ops_cached"] = ConversationStoreCache(
            generation: expected.snapshot.generation,
            revision: expected.snapshot.revision,
            operationEpoch: expected.snapshot.operationEpoch,
            items: expected.snapshot.items,
            state: expected.snapshot.state,
            operations: fixture.operations.map {
                ConversationOperation(
                    id: $0.id,
                    text: $0.text,
                    operationEpoch: expected.snapshot.operationEpoch,
                    status: .sending
                )
            }
        )
        let cached = try makeStore(sessionID: "ses_ops_cached", storage: storage)
        cached.receive(.message(.snapshot(expected.snapshot)))
        for record in fixture.operations {
            cached.receive(
                .message(
                    .operationResult(
                        operationId: record.id,
                        status: record.status,
                        itemId: nil,
                        reason: record.reason
                    )
                )
            )
        }
        XCTAssertEqual(cached.operations.map(\.status), [.refused, .ambiguous])
        XCTAssertEqual(cached.operations.map(\.text), fixture.operations.map(\.text))
        XCTAssertEqual(cached.operations.map(\.reason), fixture.operations.map(\.reason))
        // A refused message never reached the conversation, so only its
        // operation keeps the text; an ambiguous one may have, so its row stays.
        XCTAssertFalse(cached.items.contains { $0.id == "operation:op-refused" })
        XCTAssertTrue(cached.items.contains { $0.id == "operation:op-ambiguous" })
    }

    /// Mirrors ChatView.ConversationRow: every kind the store can publish has a row.
    private func assertPresentable(_ item: ConversationItem, caseName: String) {
        switch item.kind {
        case .message, .tool, .request, .unrecognized:
            break
        }
        _ = caseName
    }

    private static func loadExpected(in directory: URL) throws -> ExpectedFile {
        try loadJSON(directory.appendingPathComponent("expected.json"))
    }

    private static func loadJSON<T: Decodable>(_ url: URL) throws -> T {
        let decoder = JSONDecoder()
        return try decoder.decode(T.self, from: Data(contentsOf: url))
    }

    private static func caseDirectories() throws -> [URL] {
        try FileManager.default.contentsOfDirectory(
            at: claudeCases,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        )
        .filter { url in (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }
        .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    /// `fixtures/conversation` is repository-level and shared with the Rust
    /// connector tests, so it is reached by path rather than copied in as a
    /// test resource. Copying would let the two suites drift onto different
    /// recordings of the same session.
    private static let fixtureRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appendingPathComponent("fixtures/conversation")

    private static var claudeCases: URL { fixtureRoot.appendingPathComponent("claude/cases") }
    private static var presentation: URL { fixtureRoot.appendingPathComponent("presentation") }
}
