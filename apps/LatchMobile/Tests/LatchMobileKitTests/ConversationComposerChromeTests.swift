import LatchMobileKit
import XCTest

final class ConversationComposerChromeTests: XCTestCase {
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
