import Foundation
import XCTest

@testable import LatchMobileKit

/// Turn grouping, activity grouping, and request placement, decided without a
/// view. The captured-corpus cases use the checked-in Claude fixtures; the
/// rest pin one rule each.
final class ConversationProjectionTests: XCTestCase {
    // MARK: Turns

    func testEachUserMessageOpensATurnAndCarriesEverythingAfterIt() {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            tool("t1", 3, name: "Read"),
            user("u2", 4),
            assistant("a2", 5),
        ])

        XCTAssertEqual(result.turns.map(\.id), ["u1", "u2"])
        XCTAssertEqual(result.turns[0].prompt?.id, "u1")
        XCTAssertEqual(result.turns[0].entries.map(\.id), ["a1", "t1"])
        XCTAssertEqual(result.turns[1].entries.map(\.id), ["a2"])
        XCTAssertEqual(result.turns.map(\.isNewest), [false, true])
    }

    func testAWindowThatStartsMidTurnHasATurnWithoutAPrompt() {
        let result = project([assistant("a0", 7), tool("t0", 8), user("u1", 9)])

        XCTAssertEqual(result.turns.count, 2)
        XCTAssertNil(result.turns[0].prompt)
        XCTAssertEqual(result.turns[0].id, "a0")
        XCTAssertEqual(result.turns[0].entries.map(\.id), ["a0", "t0"])
        XCTAssertTrue(result.turns[1].entries.isEmpty)
    }

    func testItemsAreOrderedByOrdinalNotArrivalOrCreatedAt() {
        let result = project([assistant("a1", 3), user("u1", 1), tool("t1", 2)])

        XCTAssertEqual(result.turns.map(\.id), ["u1"])
        XCTAssertEqual(result.turns[0].entries.map(\.id), ["t1", "a1"])
    }

    func testEmptyItemsProduceNoTurns() {
        XCTAssertEqual(project([]), .empty)
    }

    // MARK: Activity grouping

    func testAdjacentParentlessToolsShareOneGroupInOrdinalOrder() throws {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            tool("t1", 3, name: "Read"),
            tool("t2", 4, name: "Read"),
            tool("t3", 5, name: "Bash"),
            assistant("a2", 6),
        ])

        let entries = result.turns[0].entries
        XCTAssertEqual(entries.map(\.id), ["a1", "t1", "a2"])
        let group = try activity(entries[1])
        XCTAssertEqual(group.tools.map(\.id), ["t1", "t2", "t3"])
        XCTAssertNil(group.parentMessageId)
        XCTAssertEqual(group.summary, "Ran 3 tools")
    }

    func testAMessageBetweenParentlessToolsSeparatesTheirGroups() {
        let result = project([
            user("u1", 1),
            tool("t1", 2),
            assistant("a1", 3),
            tool("t2", 4),
        ])

        XCTAssertEqual(result.turns[0].entries.map(\.id), ["t1", "a1", "t2"])
    }

    func testToolsGroupUnderTheirParentMessageEvenWhenNotAdjacent() throws {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            tool("t1", 3, parent: "a1"),
            assistant("a2", 4),
            tool("t2", 5, parent: "a1"),
            tool("t3", 6, parent: "a2"),
        ])

        let entries = result.turns[0].entries
        XCTAssertEqual(entries.map(\.id), ["a1", "t1", "a2", "t3"])
        XCTAssertEqual(try activity(entries[1]).tools.map(\.id), ["t1", "t2"])
        XCTAssertEqual(try activity(entries[1]).parentMessageId, "a1")
        XCTAssertEqual(try activity(entries[3]).tools.map(\.id), ["t3"])
        XCTAssertEqual(try activity(entries[3]).parentMessageId, "a2")
    }

    func testAParentOutsideTheTurnFallsBackToAdjacency() throws {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            user("u2", 3),
            tool("t1", 4, parent: "a1"),
            tool("t2", 5),
        ])

        XCTAssertEqual(result.turns[0].entries.map(\.id), ["a1"])
        let entries = result.turns[1].entries
        XCTAssertEqual(entries.map(\.id), ["t1"])
        XCTAssertEqual(try activity(entries[0]).tools.map(\.id), ["t1", "t2"])
    }

    func testToolsParentedByThePromptGroupAtTheStartOfTheTurn() throws {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            tool("t1", 3, parent: "u1"),
        ])

        let entries = result.turns[0].entries
        XCTAssertEqual(entries.map(\.id), ["t1", "a1"])
        XCTAssertEqual(try activity(entries[0]).tools.map(\.id), ["t1"])
    }

    func testFailedToolsAreStatusesInsideTheRun() throws {
        let result = project([
            user("u1", 1),
            tool("t1", 2, status: "failed"),
            tool("t2", 3, status: "succeeded"),
            assistant("a1", 4),
        ])

        let entries = result.turns[0].entries
        XCTAssertEqual(entries.count, 2)
        let group = try activity(entries[0])
        XCTAssertEqual(group.tools.map(\.status), [.failed, .succeeded])
        XCTAssertEqual(group.failedCount, 1)
        XCTAssertEqual(group.summary, "Ran 2 tools, 1 failed")
    }

    func testARunningToolNamesTheLiveActivityLine() throws {
        let result = project([
            user("u1", 1),
            tool("t1", 2, name: "Read", status: "succeeded"),
            tool("t2", 3, name: "Bash", status: "running"),
        ])

        let group = try activity(result.turns[0].entries[0])
        XCTAssertTrue(group.isRunning)
        XCTAssertEqual(group.runningTool?.id, "t2")
        XCTAssertEqual(group.summary, "Running Bash")
    }

    func testAnUnknownToolStatusIsSettledNotRunning() throws {
        let group = try activity(project([tool("t1", 1, status: "cancelled")]).turns[0].entries[0])

        XCTAssertEqual(group.tools[0].status, .other("cancelled"))
        XCTAssertFalse(group.isRunning)
        XCTAssertEqual(group.summary, "Ran 1 tool")
    }

    // MARK: Requests

    func testARequestStaysAtItsOrdinalAndSplitsActivity() {
        let result = project(
            [user("u1", 1), tool("t1", 2), request("r1", 3, requestId: "req-1"), tool("t2", 4)],
            state: state(pendingRequest: "req-1")
        )

        XCTAssertEqual(result.turns[0].entries.map(\.id), ["t1", "r1", "t2"])
        XCTAssertEqual(result.pendingRequest?.id, "r1")
        XCTAssertEqual(result.pendingRequest?.requestId, "req-1")
    }

    func testOnlyTheRequestNamedByStateAwaitsAnAnswer() throws {
        let result = project(
            [
                request("r1", 1, requestId: "req-old"),
                request("r2", 2, requestId: "req-new"),
                request("r3", 3, requestId: "req-done", status: "resolved"),
            ],
            state: state(pendingRequest: "req-new")
        )

        let requests = result.turns[0].entries.compactMap { entry -> ConversationRequestPresentation? in
            if case .request(let request) = entry { return request }
            return nil
        }
        XCTAssertEqual(requests.map(\.isAwaitingAnswer), [false, true, false])
        XCTAssertEqual(requests.map(\.status), [.pending, .pending, .resolved])
        XCTAssertEqual(result.pendingRequest?.requestId, "req-new")
    }

    func testNoPendingRequestWithoutState() {
        let result = project([request("r1", 1, requestId: "req-1")], state: nil)
        XCTAssertNil(result.pendingRequest)
    }

    func testAResolvedRequestWhoseIDStateStillNamesIsNotOffered() {
        let result = project(
            [request("r1", 1, requestId: "req-1", status: "resolved")],
            state: state(pendingRequest: "req-1")
        )
        XCTAssertNil(result.pendingRequest)
    }

    // MARK: Unrecognized

    func testUnrecognizedItemsKeepTheirPlace() {
        let result = project([
            user("u1", 1),
            tool("t1", 2),
            ConversationItem(id: "x", ordinal: 3, createdAt: "", kind: .unrecognized(type: "image")),
            tool("t2", 4),
        ])

        XCTAssertEqual(result.turns[0].entries[1], .unrecognized(id: "x", type: "image"))
        XCTAssertEqual(result.turns[0].entries.map(\.id), ["t1", "x", "t2"])
    }

    // MARK: Scroll targets

    func testRowIDMapsAToolToItsGroupAndOtherItemsToThemselves() {
        let result = project([
            user("u1", 1),
            assistant("a1", 2),
            tool("t1", 3),
            tool("t2", 4),
        ])

        XCTAssertEqual(result.rowID(containing: "u1"), "u1")
        XCTAssertEqual(result.rowID(containing: "a1"), "a1")
        XCTAssertEqual(result.rowID(containing: "t2"), "t1")
        XCTAssertNil(result.rowID(containing: "missing"))
    }

    // MARK: Contract values

    /// Every schema value maps to a known case; nothing the Hub can send
    /// today falls through to `.other`.
    func testEverySchemaEnumValueHasAKnownPresentation() throws {
        let enums = try ConversationSchema.itemEnums()
        for role in try XCTUnwrap(enums["message"]?["role"]) {
            XCTAssertNotEqual(ConversationProjection.messageRole(role), .other(role), role)
        }
        for status in try XCTUnwrap(enums["tool"]?["status"]) {
            XCTAssertNotEqual(ConversationProjection.toolStatus(status), .other(status), status)
        }
        for type in try XCTUnwrap(enums["request"]?["requestType"]) {
            XCTAssertNotEqual(ConversationProjection.requestKind(type), .other(type), type)
        }
        for status in try XCTUnwrap(enums["request"]?["status"]) {
            XCTAssertNotEqual(ConversationProjection.requestStatus(status), .other(status), status)
        }
    }

    // MARK: Captured corpus

    func testCapturedCorpusProjectsEveryItemExactlyOnce() throws {
        for directory in try ConversationSchema.claudeCaseDirectories() {
            let snapshot = try ConversationSchema.snapshot(in: directory)
            let result = ConversationProjection.project(items: snapshot.items, state: snapshot.state)
            XCTAssertEqual(
                Self.flattenedIDs(result),
                snapshot.items.sorted { $0.ordinal < $1.ordinal }.map(\.id),
                directory.lastPathComponent
            )
        }
    }

    /// The scroll-measurement corpus has hundreds of parentless tool calls;
    /// adjacency grouping must collapse them into far fewer disclosures.
    func testLongTranscriptCollapsesToolRunsIntoGroups() throws {
        let snapshot = try ConversationSchema.snapshot(in: ConversationSchema.claudeCases.appendingPathComponent("long-transcript"))
        let result = ConversationProjection.project(items: snapshot.items, state: snapshot.state)

        let tools = snapshot.items.filter { if case .tool = $0.kind { return true } else { return false } }.count
        let groups = result.turns.flatMap(\.entries).filter { if case .activity = $0 { return true } else { return false } }
        XCTAssertGreaterThan(tools, 100)
        XCTAssertLessThan(groups.count, tools / 3)
        XCTAssertEqual(result.turns.filter { $0.prompt != nil }.count, 14)
    }

    func testPermissionCaseOffersTheCapturedRequestOnce() throws {
        let snapshot = try ConversationSchema.snapshot(in: ConversationSchema.claudeCases.appendingPathComponent("permission-request"))
        let pendingID = try XCTUnwrap(snapshot.state.pendingRequest ?? Self.firstRequestID(snapshot.items))
        let state = ConversationState(
            phase: "awaiting_input",
            sendMessage: snapshot.state.sendMessage,
            resolveRequest: snapshot.state.resolveRequest,
            pendingRequest: pendingID,
            connector: snapshot.state.connector
        )
        let result = ConversationProjection.project(items: snapshot.items, state: state)

        let offered = result.turns.flatMap(\.entries).filter {
            if case .request(let request) = $0 { return request.isAwaitingAnswer }
            return false
        }
        XCTAssertEqual(offered.count, 1)
        XCTAssertEqual(result.pendingRequest?.requestId, pendingID)
        XCTAssertEqual(result.pendingRequest?.kind, .permission)
    }

    // MARK: Helpers

    private static func flattenedIDs(_ result: ConversationTranscriptPresentation) -> [String] {
        result.turns.flatMap { turn -> [String] in
            let prompt = turn.prompt.map { [$0.id] } ?? []
            return prompt + turn.entries.flatMap { entry -> [String] in
                if case .activity(let group) = entry { return group.tools.map(\.id) }
                return [entry.id]
            }
        }
    }

    private static func firstRequestID(_ items: [ConversationItem]) -> String? {
        for item in items {
            if case .request(let requestId, _, _, _, _) = item.kind { return requestId }
        }
        return nil
    }

    private func project(_ items: [ConversationItem], state: ConversationState? = nil) -> ConversationTranscriptPresentation {
        ConversationProjection.project(items: items, state: state)
    }

    private func activity(_ entry: ConversationTurnEntry, file: StaticString = #filePath, line: UInt = #line) throws -> ConversationActivityGroup {
        guard case .activity(let group) = entry else {
            XCTFail("expected an activity group, got \(entry)", file: file, line: line)
            throw XCTSkip("not an activity group")
        }
        return group
    }

    private func user(_ id: String, _ ordinal: UInt64) -> ConversationItem {
        ConversationItem(id: id, ordinal: ordinal, createdAt: "", kind: .message(role: "user", text: id, status: .observed))
    }

    private func assistant(_ id: String, _ ordinal: UInt64) -> ConversationItem {
        ConversationItem(id: id, ordinal: ordinal, createdAt: "", kind: .message(role: "assistant", text: id, status: .complete))
    }

    private func tool(_ id: String, _ ordinal: UInt64, name: String = "Tool", status: String = "succeeded", parent: String? = nil) -> ConversationItem {
        ConversationItem(id: id, ordinal: ordinal, createdAt: "", kind: .tool(name: name, summary: "", status: status, parentMessageId: parent))
    }

    private func request(_ id: String, _ ordinal: UInt64, requestId: String, status: String = "pending") -> ConversationItem {
        ConversationItem(
            id: id,
            ordinal: ordinal,
            createdAt: "",
            kind: .request(requestId: requestId, requestType: "permission", prompt: "Allow?", choices: ["Yes", "No"], status: status)
        )
    }

    private func state(pendingRequest: String?) -> ConversationState {
        ConversationState(
            phase: pendingRequest == nil ? "idle" : "awaiting_input",
            sendMessage: OperationAvailability(enabled: pendingRequest == nil, reason: nil),
            resolveRequest: OperationAvailability(enabled: pendingRequest != nil, reason: nil),
            pendingRequest: pendingRequest,
            connector: nil
        )
    }
}

/// Shared fixture and schema access for the presentation tests.
enum ConversationSchema {
    static let app: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // LatchMobileKitTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // LatchMobile

    static let repository: URL = app
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // repository

    static var claudeCases: URL { repository.appendingPathComponent("fixtures/conversation/claude/cases") }

    static func claudeCaseDirectories() throws -> [URL] {
        try FileManager.default.contentsOfDirectory(at: claudeCases, includingPropertiesForKeys: [.isDirectoryKey])
            .filter { (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    private struct ExpectedFile: Decodable { let snapshot: ConversationSnapshot }

    static func snapshot(in directory: URL) throws -> ConversationSnapshot {
        let data = try Data(contentsOf: directory.appendingPathComponent("expected.json"))
        return try JSONDecoder().decode(ExpectedFile.self, from: data).snapshot
    }

    /// `kind` → field → allowed values, from the canonical item schema.
    static func itemEnums() throws -> [String: [String: Set<String>]] {
        let data = try Data(contentsOf: repository.appendingPathComponent("schemas/remote-access/v2/conversation-item.schema.json"))
        let schema = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let properties = try XCTUnwrap(schema["properties"] as? [String: Any])
        let kind = try XCTUnwrap(properties["kind"] as? [String: Any])
        let variants = try XCTUnwrap(kind["oneOf"] as? [[String: Any]])
        var results: [String: [String: Set<String>]] = [:]
        for variant in variants {
            let variantProperties = try XCTUnwrap(variant["properties"] as? [String: Any])
            let type = try XCTUnwrap(variantProperties["type"] as? [String: Any])
            let kindName = try XCTUnwrap(type["const"] as? String)
            for (field, definition) in variantProperties {
                guard let definition = definition as? [String: Any],
                      let values = definition["enum"] as? [String]
                else { continue }
                results[kindName, default: [:]][field] = Set(values)
            }
        }
        return results
    }

    /// The allowed `ConversationState.phase` values.
    static func phases() throws -> Set<String> {
        let data = try Data(contentsOf: repository.appendingPathComponent("schemas/remote-access/v2/conversation-state.schema.json"))
        let schema = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let properties = try XCTUnwrap(schema["properties"] as? [String: Any])
        let phase = try XCTUnwrap(properties["phase"] as? [String: Any])
        return Set(try XCTUnwrap(phase["enum"] as? [String]))
    }
}
