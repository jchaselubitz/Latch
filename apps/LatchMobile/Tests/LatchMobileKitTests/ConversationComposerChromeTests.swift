@testable import LatchMobileKit
import XCTest

final class ConversationComposerChromeTests: XCTestCase {
    private let catalog = [
        AdvertisedCommand(name: "compact", description: "Compact history", source: "builtin"),
        AdvertisedCommand(name: "plugin:review", description: "Review changes", source: "plugin"),
    ]

    func testSlashSuggestionsRequireAdvertisedCatalogAndCommandToken() {
        func names(_ draft: String, catalog: [AdvertisedCommand]? = nil) -> [String] {
            ConversationCommandPickerPresentation(
                draft: draft, catalog: catalog ?? self.catalog, canSelect: true
            ).commands.map(\.name)
        }
        XCTAssertEqual(names("/"), ["compact", "plugin:review"])
        XCTAssertEqual(names("/COM"), ["compact"])
        XCTAssertEqual(names("/plugin:"), ["plugin:review"])
        XCTAssertEqual(names("/unknown"), [])
        XCTAssertEqual(names("/", catalog: []), [], "Never invent commands when no catalog is advertised")
        for draft in ["", "hello /", " /compact", "/compact ", "/compact args", "/compact\n"] {
            XCTAssertEqual(names(draft), [], draft)
        }
    }

    func testPickingOnlyInsertsAnAvailableCommandAndLeavesArgumentsReady() {
        let picker = ConversationCommandPickerPresentation(draft: "/com", catalog: catalog, canSelect: true)
        XCTAssertEqual(picker.inserting(catalog[0]), "/compact ")
        XCTAssertNil(picker.inserting(catalog[1]), "A stale filtered-out row cannot insert")
        let busy = ConversationCommandPickerPresentation(draft: "/", catalog: catalog, canSelect: false)
        XCTAssertEqual(busy.commands, catalog, "Busy suggestions remain visible but disabled")
        XCTAssertNil(busy.inserting(catalog[0]))
        let gone = ConversationCommandPickerPresentation(draft: "/", catalog: [], canSelect: true)
        XCTAssertNil(gone.inserting(catalog[0]), "A withdrawn catalog removes its actions")
    }

    func testQueuedMessageAvailabilityDoesNotEnableSlashCommandsDuringATurn() throws {
        func state(_ phase: String, pending: String? = nil) throws -> ConversationState {
            let json: [String: Any] = [
                "phase": phase, "sendMessage": ["enabled": true],
                "resolveRequest": ["enabled": false], "pendingRequest": pending as Any? ?? NSNull(),
            ]
            return try JSONDecoder().decode(ConversationState.self, from: JSONSerialization.data(withJSONObject: json))
        }
        let idle = try state("idle")
        for viewState in ConversationViewState.allCases {
            XCTAssertEqual(ConversationComposerChrome.canUseSlashCommands(
                viewState: viewState, state: idle, canSend: true
            ), viewState == .ready || viewState == .empty)
        }
        for phase in ["working", "awaiting_input", "starting", "exited", "unavailable", "future_phase"] {
            XCTAssertFalse(ConversationComposerChrome.canUseSlashCommands(
                viewState: .ready, state: try state(phase), canSend: true
            ), phase)
        }
        XCTAssertFalse(ConversationComposerChrome.canUseSlashCommands(viewState: .ready, state: nil, canSend: true))
        XCTAssertFalse(ConversationComposerChrome.canUseSlashCommands(viewState: .ready, state: idle, canSend: false))
        XCTAssertFalse(ConversationComposerChrome.canUseSlashCommands(
            viewState: .ready, state: try state("idle", pending: "permission"), canSend: true
        ))
    }

    func testAnEmptyConversationThatCanSendTakesFocus() {
        XCTAssertTrue(ConversationComposerChrome.shouldFocusOnOpen(canSend: true, transcript: .empty))
        XCTAssertFalse(ConversationComposerChrome.shouldFocusOnOpen(canSend: false, transcript: .empty))
    }

    func testFocusFollowsWhoseTurnIsNewest() {
        let user = message("u1", 1, .user)
        let agent = message("a1", 2, .assistant)

        let userLast = ConversationTranscriptPresentation(turns: [
            ConversationTurnPresentation(id: "t1", prompt: user, entries: [], isNewest: true),
        ])
        XCTAssertTrue(ConversationComposerChrome.shouldFocusOnOpen(canSend: true, transcript: userLast))
        XCTAssertFalse(ConversationComposerChrome.shouldFocusOnOpen(canSend: false, transcript: userLast))

        let agentLast = ConversationTranscriptPresentation(turns: [
            ConversationTurnPresentation(id: "t1", prompt: user, entries: [.message(agent)], isNewest: true),
        ])
        XCTAssertFalse(ConversationComposerChrome.shouldFocusOnOpen(canSend: true, transcript: agentLast))

        let unplaced = ConversationTranscriptPresentation(turns: [
            ConversationTurnPresentation(id: "t1", prompt: user, entries: [.unrecognized(id: "x", type: "future")], isNewest: true),
        ])
        XCTAssertFalse(ConversationComposerChrome.shouldFocusOnOpen(canSend: true, transcript: unplaced))
    }

    private func message(_ id: String, _ ordinal: UInt64, _ role: ConversationMessageRole) -> ConversationMessagePresentation {
        ConversationMessagePresentation(id: id, ordinal: ordinal, role: role, text: "hi", status: .complete)
    }
}
