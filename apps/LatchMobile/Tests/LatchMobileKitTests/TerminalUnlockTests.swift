import XCTest

@testable import LatchMobileKit

/// A device-owner check with a scripted answer, so the rules around the prompt
/// can be asserted without one.
final class StubDeviceOwnerAuthenticator: DeviceOwnerAuthenticating, @unchecked Sendable {
    private let lock = NSLock()
    private var _available: Bool
    private var _approves: Bool
    private var _failure: Error?
    private var _prompts = 0

    init(available: Bool = true, approves: Bool = true, failure: Error? = nil) {
        _available = available
        _approves = approves
        _failure = failure
    }

    /// How many times the owner was actually asked. The point of the grace
    /// window is that this does not go up on every attach.
    var prompts: Int { lock.withLock { _prompts } }

    func set(approves: Bool) { lock.withLock { _approves = approves } }

    func canAuthenticate() -> Bool { lock.withLock { _available } }

    func authenticate(reason: String) async throws -> Bool {
        let (failure, approves): (Error?, Bool) = lock.withLock {
            _prompts += 1
            return (_failure, _approves)
        }
        if let failure { throw failure }
        return approves
    }
}

@MainActor
final class TerminalUnlockTests: XCTestCase {
    private enum Broken: Error { case now }

    func testAPassedCheckOpensTheTerminalAndHoldsForTheGraceWindow() async {
        let clock = MutableClock()
        let authenticator = StubDeviceOwnerAuthenticator()
        let unlock = TerminalUnlock(
            authenticator: authenticator,
            grace: 60,
            now: { clock.now }
        )

        XCTAssertFalse(unlock.isUnlocked)
        let opened = await unlock.unlock(reason: "test")
        XCTAssertTrue(opened)
        XCTAssertTrue(unlock.isUnlocked)
        XCTAssertEqual(authenticator.prompts, 1)

        // Inside the window: no second prompt, which is the whole reason a
        // person can attach, read something else, and reattach.
        clock.advance(30)
        let answer1 = await unlock.unlock(reason: "test")
        XCTAssertTrue(answer1)
        XCTAssertEqual(authenticator.prompts, 1)

        // Past it: asked again.
        clock.advance(31)
        XCTAssertFalse(unlock.isUnlocked)
        let answer2 = await unlock.unlock(reason: "test")
        XCTAssertTrue(answer2)
        XCTAssertEqual(authenticator.prompts, 2)
    }

    /// A cancelled prompt is an answer, not a malfunction: the terminal stays
    /// shut and nothing is reported, because the person just declined it.
    func testACancelledCheckLeavesTheTerminalShutAndSaysNothing() async {
        let authenticator = StubDeviceOwnerAuthenticator(approves: false)
        let unlock = TerminalUnlock(authenticator: authenticator, grace: 60)

        let answer1 = await unlock.unlock(reason: "test")
        XCTAssertFalse(answer1)
        XCTAssertFalse(unlock.isUnlocked)
        XCTAssertNil(unlock.failure)
    }

    /// A device with no passcode has no owner check to run, so the terminal is
    /// refused rather than opened without one.
    func testADeviceWithNoPasscodeIsRefusedRatherThanWavedThrough() async {
        let unlock = TerminalUnlock(
            authenticator: StubDeviceOwnerAuthenticator(available: false),
            grace: 60
        )

        let answer1 = await unlock.unlock(reason: "test")
        XCTAssertFalse(answer1)
        XCTAssertFalse(unlock.isUnlocked)
        XCTAssertNotNil(unlock.failure)
    }

    func testACheckThatCouldNotRunIsReportedAndDoesNotOpenTheTerminal() async {
        let unlock = TerminalUnlock(
            authenticator: StubDeviceOwnerAuthenticator(failure: Broken.now),
            grace: 60
        )

        let answer1 = await unlock.unlock(reason: "test")
        XCTAssertFalse(answer1)
        XCTAssertFalse(unlock.isUnlocked)
        XCTAssertNotNil(unlock.failure)
    }

    func testLockingEndsTheGraceWindow() async {
        let authenticator = StubDeviceOwnerAuthenticator()
        let unlock = TerminalUnlock(authenticator: authenticator, grace: 600)

        let answer1 = await unlock.unlock(reason: "test")
        XCTAssertTrue(answer1)
        unlock.lock()
        XCTAssertFalse(unlock.isUnlocked)
        let answer2 = await unlock.unlock(reason: "test")
        XCTAssertTrue(answer2)
        XCTAssertEqual(authenticator.prompts, 2)
    }
}

/// A clock the test moves by hand, so a grace window can expire without one.
final class MutableClock: @unchecked Sendable {
    private let lock = NSLock()
    private var value = Date(timeIntervalSince1970: 1_700_000_000)

    var now: Date { lock.withLock { value } }

    func advance(_ seconds: TimeInterval) {
        lock.withLock { value = value.addingTimeInterval(seconds) }
    }
}
