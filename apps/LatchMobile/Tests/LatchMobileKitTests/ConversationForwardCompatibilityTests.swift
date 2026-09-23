import XCTest

@testable import LatchMobileKit

/// The installed client has to survive content a newer gateway sends. These
/// pin the fail-open contract: an item this build cannot present becomes a
/// placeholder row, the rest of the batch still renders, the degradation is
/// counted, and nothing about it drops or reconnects the socket.
final class ConversationForwardCompatibilityTests: XCTestCase {
    /// Serves its frames once, then stays open until cancelled, so any
    /// reconnect in these tests can only come from a decode failure.
    private final class FramesThenIdle: ConversationSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var frames: [Data]

        init(_ frames: [String]) { self.frames = frames.map { Data($0.utf8) } }

        func receive() async throws -> Data {
            let next: Data? = lock.withLock { frames.isEmpty ? nil : frames.removeFirst() }
            if let next { return next }
            try await Task.sleep(for: .seconds(60))
            throw CancellationError()
        }

        func send(_ text: String) async throws {}
        func cancel() {}
    }

    private actor Recorder {
        var connections = 0
        var events: [ConversationSocketEvent] = []
        func connected() { connections += 1 }
        func record(_ event: ConversationSocketEvent) { events.append(event) }
    }

    private static let state = """
    "state":{"phase":"ready","sendMessage":{"enabled":true},"resolveRequest":{"enabled":false},\
    "pendingRequest":null,"connector":null}
    """

    private static func message(_ id: String, _ ordinal: Int, _ text: String) -> String {
        """
        {"id":"\(id)","ordinal":\(ordinal),"createdAt":"2026-09-21T00:00:00Z",\
        "kind":{"type":"message","role":"assistant","text":"\(text)","status":"complete"}}
        """
    }

    /// A fourth `kind.type` in the middle of a snapshot.
    private static let snapshotWithUnknownKind = """
    {"type":"snapshot","generation":"g","revision":1,"operationEpoch":"e","items":[\
    \(message("m1", 1, "before")),\
    {"id":"rich1","ordinal":2,"createdAt":"2026-09-21T00:00:01Z",\
    "kind":{"type":"artifact","title":"Plan","blocks":[{"type":"markdown","text":"# Hi"}]}},\
    \(message("m3", 3, "after"))],\(state),"hasMoreBefore":false,"reason":"initial"}
    """

    /// A known `tool` missing required fields, an element whose id is
    /// unreadable, and a null element, among good items.
    private static let upsertWithMalformedItems = """
    {"type":"items_upserted","generation":"g","revision":2,"items":[\
    {"id":"t1","ordinal":4,"createdAt":"2026-09-21T00:00:02Z","kind":{"type":"tool","name":"Bash"}},\
    {"ordinal":5,"kind":{"type":"message"}},\
    null,\
    {"id":"odd","ordinal":6},\
    \(message("m7", 7, "still here"))]}
    """

    private func run(_ frames: [String]) async throws -> (connections: Int, events: [ConversationSocketEvent]) {
        let recorder = Recorder()
        let socket = ConversationSocket(
            makeConnection: { _ in
                await recorder.connected()
                return FramesThenIdle(frames)
            },
            eventHandler: { event in await recorder.record(event) }
        )
        await socket.start(position: ConversationResumePosition())
        try await Task.sleep(for: .milliseconds(150))
        await socket.stop()
        return (await recorder.connections, await recorder.events)
    }

    private func messages(_ events: [ConversationSocketEvent]) -> [ConversationServerMessage] {
        events.compactMap { if case .message(let message) = $0 { return message } else { return nil } }
    }

    private func diagnostics(_ events: [ConversationSocketEvent]) -> ConversationDecodeDiagnostics {
        events.reduce(ConversationDecodeDiagnostics()) { total, event in
            if case .degraded(let diagnostics) = event { return total + diagnostics }
            return total
        }
    }

    private func assertStayedOpen(_ run: (connections: Int, events: [ConversationSocketEvent]), file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(run.connections, 1, "a decode failure must never trigger a reconnect", file: file, line: line)
        XCTAssertFalse(run.events.contains { if case .failure = $0 { return true } else { return false } }, file: file, line: line)
        XCTAssertFalse(run.events.contains { if case .state(.reconnecting) = $0 { return true } else { return false } }, file: file, line: line)
    }

    func testUnknownKindKeepsSocketOpenAndBecomesPlaceholderInPlace() async throws {
        let result = try await run([Self.snapshotWithUnknownKind])
        assertStayedOpen(result)

        guard case .snapshot(let snapshot) = messages(result.events).first else {
            return XCTFail("the snapshot should still be delivered")
        }
        XCTAssertEqual(snapshot.items.map(\.id), ["m1", "rich1", "m3"])
        XCTAssertEqual(snapshot.items[1].ordinal, 2)
        XCTAssertEqual(snapshot.items[1].kind, .unrecognized(type: "artifact"))
        XCTAssertEqual(snapshot.items[2].kind, .message(role: "assistant", text: "after", status: .complete))
        XCTAssertEqual(diagnostics(result.events), ConversationDecodeDiagnostics(unrecognizedItems: 1))
    }

    func testMalformedKnownItemDegradesTheSameWay() async throws {
        let result = try await run([Self.snapshotWithUnknownKind, Self.upsertWithMalformedItems])
        assertStayedOpen(result)

        let delivered = messages(result.events)
        XCTAssertEqual(delivered.count, 2)
        guard case .itemsUpserted(_, 2, let items) = delivered.last else {
            return XCTFail("the upsert should still be delivered")
        }
        XCTAssertEqual(items.map(\.id), ["t1", "odd", "m7"])
        XCTAssertEqual(items[0].kind, .unrecognized(type: "tool"))
        XCTAssertEqual(items[0].ordinal, 4)
        XCTAssertEqual(items[1].kind, .unrecognized(type: nil))
        XCTAssertEqual(items[1].createdAt, "")
        XCTAssertEqual(items[2].kind, .message(role: "assistant", text: "still here", status: .complete))
        XCTAssertEqual(
            diagnostics(result.events),
            ConversationDecodeDiagnostics(unrecognizedItems: 3, droppedItems: 2)
        )
    }

    func testUndecodableFrameIsSkippedWithoutReconnecting() async throws {
        let result = try await run([
            #"{"type":"presence_changed","generation":"g","who":"desk"}"#,
            "not json",
            Self.snapshotWithUnknownKind
        ])
        assertStayedOpen(result)
        XCTAssertEqual(messages(result.events).count, 1)
        XCTAssertEqual(diagnostics(result.events).undecodableFrames, 2)
    }

    @MainActor
    func testStoreRendersPlaceholderCountsDegradationAndCachesIt() throws {
        final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
            var cache: ConversationStoreCache?
            func load(sessionID: String) throws -> ConversationStoreCache? { cache }
            func save(_ cache: ConversationStoreCache, sessionID: String) throws { self.cache = cache }
        }
        let storage = MemoryStorage()
        let gateway = LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: ""))
        let store = ConversationStore(sessionID: "ses_fc", gateway: gateway, operationRetentionSeconds: 60, storage: storage)

        let decoder = JSONDecoder()
        let tally = ConversationDecodeTally()
        decoder.userInfo[.conversationDecodeTally] = tally
        let snapshot = try decoder.decode(ConversationServerMessage.self, from: Data(Self.snapshotWithUnknownKind.utf8))
        store.receive(.degraded(ConversationDecodeDiagnostics(unrecognizedItems: tally.unrecognizedItems)))
        store.receive(.message(snapshot))

        XCTAssertEqual(store.items.map(\.id), ["m1", "rich1", "m3"])
        XCTAssertEqual(store.decodeDiagnostics.unrecognizedItems, 1)

        // The placeholder survives the disk cache as a placeholder, so a
        // cached unrecognized item cannot fail the next cold open.
        let data = try JSONEncoder().encode(try XCTUnwrap(storage.cache))
        let restored = try JSONDecoder().decode(ConversationStoreCache.self, from: data)
        XCTAssertEqual(restored.items.map(\.kind)[1], .unrecognized(type: "artifact"))
        XCTAssertEqual(restored.items.count, 3)
    }
}
