import Foundation
import XCTest
@testable import LatchMobileKit

final class RichConversationContractTests: XCTestCase {
    func testRequestsDecodeLegacyAndPreserveUnknownKinds() throws {
        let legacy = """
        {"type":"request","requestId":"r","requestType":"permission","prompt":"Continue?",
         "choices":["Yes","No"],"status":"pending"}
        """
        let rich = """
        {"type":"request","requestId":"r","requestType":"permission","prompt":"Continue?",
         "choices":["Yes","No"],"status":"resolved","requestKind":"future_request",
         "details":{"hint":"hello"},"options":[{"id":"allow_once","label":"Yes"}],
         "answerable":false,"reason":"At the terminal","resolvedBy":"terminal","decision":"allow_once"}
        """
        let decoder = JSONDecoder()
        guard case .request(_, _, let oldPrompt, let oldChoices, _, _, let legacyMetadata) = try decoder.decode(
            ConversationItemKind.self, from: Data(legacy.utf8)
        ) else { return XCTFail("Expected a legacy request") }
        XCTAssertEqual(oldPrompt, "Continue?")
        XCTAssertEqual(oldChoices, ["Yes", "No"])
        XCTAssertNil(legacyMetadata)

        let kind = try decoder.decode(ConversationItemKind.self, from: Data(rich.utf8))
        guard case .request(_, _, let prompt, let choices, "resolved", _, let metadata) = kind else {
            return XCTFail("Expected a rich request")
        }
        XCTAssertEqual(prompt, oldPrompt)
        XCTAssertEqual(choices, oldChoices)
        XCTAssertEqual(metadata?.requestKind, "future_request")
        XCTAssertEqual(metadata?.options?.first?.id, "allow_once")
        XCTAssertEqual(metadata?.answerable, false)
        XCTAssertEqual(metadata?.resolvedBy, "terminal")
        XCTAssertEqual(metadata?.decision, "allow_once")
        XCTAssertEqual(try decoder.decode(ConversationItemKind.self, from: JSONEncoder().encode(kind)), kind)
    }

    func testActivitiesAndStateDecodeAndNestWithAnUnknownSibling() throws {
        let frame = """
        {"type":"snapshot","generation":"g","revision":1,"operationEpoch":"epoch",
         "hasMoreBefore":false,
         "state":{"phase":"working","sendMessage":{"enabled":false},"resolveRequest":{"enabled":false},
                  "backgroundWork":{"count":2,"kinds":["subagent","tool"]},
                  "model":"model-a","effort":"high","permissionMode":"ask"},
         "items":[
           {"id":"agent","ordinal":1,"createdAt":"now","kind":{"type":"subagent","subagentId":"a",
            "agentType":"Explore","status":"running","summary":"Reading","parentToolId":"tool-a"}},
           {"id":"tasks","ordinal":2,"createdAt":"now","parentSubagentId":"agent","kind":{"type":"task_list",
            "tasks":[{"id":"t","title":"Review","status":"running"}]}},
           {"id":"plan","ordinal":3,"createdAt":"now","kind":{"type":"plan_update",
            "steps":[{"id":"s","title":"Build","status":"completed"}]}},
           {"id":"reason","ordinal":4,"createdAt":"now","kind":{"type":"reasoning_summary","text":"Compare options"}},
           {"id":"notice","ordinal":5,"createdAt":"now","kind":{"type":"notice","noticeKind":"compaction","text":"Compacted"}},
           {"id":"edit","ordinal":6,"createdAt":"now","kind":{"type":"file_change",
            "path":"source.swift","diffSummary":"+ test","toolItemId":"tool-b"}},
           {"id":"unknown","ordinal":7,"createdAt":"now","kind":{"type":"future_activity"}}
         ]}
        """
        let decoder = JSONDecoder()
        guard case .snapshot(let snapshot) = try decoder.decode(ConversationServerMessage.self, from: Data(frame.utf8)) else {
            return XCTFail("Expected a snapshot")
        }
        XCTAssertEqual(snapshot.state.backgroundWork?.count, 2)
        XCTAssertEqual(snapshot.state.backgroundWork?.kinds, ["subagent", "tool"])
        XCTAssertEqual(snapshot.state.model, "model-a")
        XCTAssertEqual(snapshot.state.effort, "high")
        XCTAssertEqual(snapshot.state.permissionMode, "ask")
        XCTAssertEqual(snapshot.items.last?.kind, .unrecognized(type: "future_activity"))
        let transcript = ConversationProjection.project(items: snapshot.items, state: snapshot.state)
        guard case .agentActivity(let thread) = transcript.turns.first?.entries.first else {
            return XCTFail("Expected the subagent thread")
        }
        XCTAssertEqual(thread.children.map(\.id), ["tasks"])
        XCTAssertEqual(transcript.rowID(containing: "tasks"), "agent")
        XCTAssertEqual(transcript.turns.first?.entries.map(\.id), ["agent", "plan", "reason", "notice", "edit", "unknown"])
        for item in snapshot.items {
            XCTAssertEqual(try decoder.decode(ConversationItem.self, from: JSONEncoder().encode(item)), item)
        }
    }

    func testOptionActionEncodesTypedFormWithoutLegacyChoice() throws {
        let action = ConversationClientMessage.resolveRequest(
            operationEpoch: "epoch", operationId: "op", requestId: "r", optionId: "submit_form",
            text: "Feedback", form: ["enabled": .boolean(true), "count": .integer(2)]
        )
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(action)) as? [String: Any])
        XCTAssertEqual(object["optionId"] as? String, "submit_form")
        XCTAssertEqual(object["text"] as? String, "Feedback")
        XCTAssertNil(object["choice"])
        XCTAssertNil(object["answers"])
        XCTAssertEqual((object["form"] as? [String: Any])?["count"] as? Int, 2)
    }
}
