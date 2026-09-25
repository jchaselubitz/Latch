import XCTest

@testable import LatchMobileKit

final class SessionChromePresentationTests: XCTestCase {
    func testTheChipNamesOnlyTheStatesAPersonActsOn() {
        XCTAssertEqual(TerminalChromeStatus(state: nil).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .idle).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .connecting).label, "Attaching…")
        XCTAssertEqual(TerminalChromeStatus(state: .attached).label, "Attached")
        XCTAssertEqual(TerminalChromeStatus(state: .interrupted(resumable: true)).label, "Reconnecting")
        XCTAssertEqual(TerminalChromeStatus(state: .interrupted(resumable: false)).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .closed(.stolen)).label, "Captured elsewhere")
        XCTAssertEqual(TerminalChromeStatus(state: .closed(.detached)).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .closed(.resumeRefused)).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .closed(nil)).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .failed("refused")).label, "Detached")
        XCTAssertEqual(TerminalChromeStatus(state: .closed(.sessionExited)).label, "Exited")
    }

    func testReattachLeadsTheSheetOnlyWhenThereIsSomethingToTakeBack() {
        XCTAssertTrue(TerminalChromeStatus(state: .idle).offersReattach)
        XCTAssertTrue(TerminalChromeStatus(state: .closed(.stolen)).offersReattach)
        XCTAssertTrue(TerminalChromeStatus(state: .interrupted(resumable: false)).offersReattach)
        XCTAssertFalse(TerminalChromeStatus(state: .attached).offersReattach)
        XCTAssertFalse(TerminalChromeStatus(state: .connecting).offersReattach)
        XCTAssertFalse(TerminalChromeStatus(state: .interrupted(resumable: true)).offersReattach)
        XCTAssertFalse(TerminalChromeStatus(state: .closed(.sessionExited)).offersReattach)
    }

    func testTheDetailsSheetNamesTheConnectorWithoutGuessing() {
        XCTAssertEqual(SessionConnector.none.detailLabel, "None (shell)")
        XCTAssertEqual(SessionConnector.unknown.detailLabel, "Not reported")
        XCTAssertEqual(SessionConnector.named("claude").detailLabel, SessionAgent(rawValue: "claude")?.displayName)
        XCTAssertEqual(SessionConnector.named("aider").detailLabel, "aider")
        XCTAssertEqual(SessionConnector.named("").detailLabel, "Not reported")
    }
}
