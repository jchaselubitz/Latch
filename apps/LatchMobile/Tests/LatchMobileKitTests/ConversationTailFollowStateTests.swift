import XCTest

@testable import LatchMobileKit

/// Tail-follow decisions, without a scroll view.
final class ConversationTailFollowStateTests: XCTestCase {
    func testAppendFollowsTheTailWhileFollowing() {
        var follow = ConversationTailFollowState()
        XCTAssertEqual(follow.itemsAppended(), .scrollToTail(animated: true))
        XCTAssertFalse(follow.showsJumpToLatest)
    }

    func testReduceMotionSuppressesAnimatedFollow() {
        var follow = ConversationTailFollowState()
        XCTAssertEqual(follow.itemsAppended(reduceMotion: true), .scrollToTail(animated: false))
        XCTAssertEqual(follow.jumpToLatest(reduceMotion: true), .scrollToTail(animated: false))
    }

    func testScrollingUpReleasesFollowAndAppendDoesNotMoveTheReader() {
        var follow = ConversationTailFollowState()
        follow.viewportMoved(distanceFromBottom: 400, isUserDriven: true)

        XCTAssertFalse(follow.isFollowing)
        XCTAssertTrue(follow.showsJumpToLatest)
        XCTAssertEqual(follow.itemsAppended(), .none)
        XCTAssertTrue(follow.hasUnseenItems)
    }

    func testSmallDriftWithinTheThresholdKeepsFollowing() {
        var follow = ConversationTailFollowState(threshold: 64)
        follow.viewportMoved(distanceFromBottom: 40, isUserDriven: true)

        XCTAssertTrue(follow.isFollowing)
        XCTAssertEqual(follow.itemsAppended(), .scrollToTail(animated: true))
    }

    func testLayoutGrowthAlonePausesNothing() {
        // A long row arriving pushes the bottom away without the reader
        // touching anything; that must not release follow.
        var follow = ConversationTailFollowState()
        follow.viewportMoved(distanceFromBottom: 900, isUserDriven: false)

        XCTAssertTrue(follow.isFollowing)
        XCTAssertEqual(follow.itemsAppended(), .scrollToTail(animated: true))
    }

    func testReturningToTheBottomByHandResumesFollowing() {
        var follow = ConversationTailFollowState()
        follow.viewportMoved(distanceFromBottom: 400, isUserDriven: true)
        _ = follow.itemsAppended()
        follow.viewportMoved(distanceFromBottom: 0, isUserDriven: true)

        XCTAssertTrue(follow.isFollowing)
        XCTAssertFalse(follow.hasUnseenItems)
        XCTAssertFalse(follow.showsJumpToLatest)
    }

    func testJumpToLatestRestoresFollowing() {
        var follow = ConversationTailFollowState()
        follow.viewportMoved(distanceFromBottom: 400, isUserDriven: true)
        _ = follow.itemsAppended()

        XCTAssertEqual(follow.jumpToLatest(), .scrollToTail(animated: true))
        XCTAssertTrue(follow.isFollowing)
        XCTAssertFalse(follow.hasUnseenItems)
        XCTAssertEqual(follow.itemsAppended(), .scrollToTail(animated: true))
    }

    func testPrependRestoresTheAnchorAndLeavesFollowUnchanged() {
        var follow = ConversationTailFollowState()
        follow.viewportMoved(distanceFromBottom: 400, isUserDriven: true)

        XCTAssertEqual(follow.historyPrepended(anchorID: "row-12"), .restoreAnchor("row-12"))
        XCTAssertFalse(follow.isFollowing)
        XCTAssertEqual(follow.historyPrepended(anchorID: nil), .none)
    }

    func testAnOlderRenderedWindowHasNoTailToFollow() {
        var follow = ConversationTailFollowState()
        XCTAssertEqual(follow.itemsAppended(tailIsRendered: false), .none)
        XCTAssertTrue(follow.hasUnseenItems)
    }
}
