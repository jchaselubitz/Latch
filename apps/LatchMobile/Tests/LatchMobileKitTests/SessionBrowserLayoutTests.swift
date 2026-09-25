import XCTest

@testable import LatchMobileKit

final class SessionBrowserLayoutTests: XCTestCase {
    func testRegularWidthWithASessionListUsesColumns() {
        XCTAssertEqual(
            SessionBrowserLayoutPolicy.layout(isRegularWidth: true, showsSessionList: true),
            .columns
        )
    }

    func testANarrowWindowKeepsTheStack() {
        XCTAssertEqual(
            SessionBrowserLayoutPolicy.layout(isRegularWidth: false, showsSessionList: true),
            .stack
        )
    }

    func testAWideWindowWithoutAListStaysOneColumn() {
        XCTAssertEqual(
            SessionBrowserLayoutPolicy.layout(isRegularWidth: true, showsSessionList: false),
            .stack
        )
        XCTAssertEqual(
            SessionBrowserLayoutPolicy.layout(isRegularWidth: false, showsSessionList: false),
            .stack
        )
    }

    func testBackIsForTheStackAndForAScreenPushedOverAColumn() {
        XCTAssertTrue(
            SessionBrowserLayoutPolicy.showsSessionBackButton(layout: .stack, isPushedOverSession: false)
        )
        XCTAssertTrue(
            SessionBrowserLayoutPolicy.showsSessionBackButton(layout: .stack, isPushedOverSession: true)
        )
        XCTAssertFalse(
            SessionBrowserLayoutPolicy.showsSessionBackButton(layout: .columns, isPushedOverSession: false)
        )
        XCTAssertTrue(
            SessionBrowserLayoutPolicy.showsSessionBackButton(layout: .columns, isPushedOverSession: true)
        )
    }
}
