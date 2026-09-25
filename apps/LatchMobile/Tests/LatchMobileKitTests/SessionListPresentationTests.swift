import XCTest

@testable import LatchMobileKit

final class SessionListPresentationTests: XCTestCase {
    private func session(
        _ id: String,
        state: String = "running",
        title: String? = nil,
        cwd: String = "/Users/me/src/latch",
        idleMs: Int? = nil,
        connector: SessionConnector = .unknown
    ) -> SessionSummary {
        SessionSummary(
            id: id,
            name: "session-\(id)",
            title: title,
            state: state,
            cwd: cwd,
            commandLabel: "zsh",
            createdAt: "2026-09-25T00:00:00Z",
            idleMs: idleMs,
            connector: connector
        )
    }

    func testRunningHoldsEverySessionWhoseProgramStillExists() {
        XCTAssertEqual(SessionListSection(state: "running"), .running)
        XCTAssertEqual(SessionListSection(state: "creating"), .running)
        XCTAssertEqual(SessionListSection(state: "stopping"), .running)
        XCTAssertEqual(SessionListSection(state: "exited"), .stopped)
        XCTAssertEqual(SessionListSection(state: "failed"), .stopped)
        XCTAssertEqual(SessionListSection(state: "something-new"), .stopped)
    }

    func testGroupsRunningFirstAndKeepTheMacsOrderWithinEach() {
        let sessions = [
            session("a", state: "exited"),
            session("b", state: "running"),
            session("c", state: "stopping"),
            session("d", state: "failed"),
            session("e", state: "creating"),
        ]
        let groups = SessionListGroup.grouped(sessions)
        XCTAssertEqual(groups.map(\.section), [.running, .stopped])
        XCTAssertEqual(groups[0].sessions.map(\.id), ["b", "c", "e"])
        XCTAssertEqual(groups[1].sessions.map(\.id), ["a", "d"])
    }

    func testAnEmptyGroupIsLeftOut() {
        XCTAssertEqual(SessionListGroup.grouped([session("a")]).map(\.section), [.running])
        XCTAssertEqual(SessionListGroup.grouped([session("a", state: "exited")]).map(\.section), [.stopped])
        XCTAssertTrue(SessionListGroup.grouped([]).isEmpty)
    }

    func testSearchFindsTitleFolderAndAgent() {
        let sessions = [
            session("title", title: "Fix the relay", cwd: "/x/alpha"),
            session("folder", cwd: "/x/Überweb"),
            session("agent", cwd: "/x/beta", connector: .named("claude")),
            session("shell", cwd: "/x/gamma", connector: .none),
        ]
        func ids(_ query: String) -> [String] {
            SessionListGroup.grouped(sessions, matching: query).flatMap(\.sessions).map(\.id)
        }
        XCTAssertEqual(ids("RELAY"), ["title"])
        XCTAssertEqual(ids("uberweb"), ["folder"])
        XCTAssertEqual(ids("claude"), ["agent"])
        if let agent = SessionAgent(rawValue: "claude") {
            XCTAssertEqual(ids(agent.displayName), ["agent"])
        }
        XCTAssertEqual(ids("shell"), ["shell"])
        XCTAssertEqual(ids("   "), ["title", "folder", "agent", "shell"])
        XCTAssertEqual(ids("nothing matches this"), [])
    }

    func testOnlyStartingAndStoppingRowsCarryADot() {
        XCTAssertTrue(session("a", state: "creating").isTransitioning)
        XCTAssertTrue(session("a", state: "stopping").isTransitioning)
        XCTAssertFalse(session("a", state: "running").isTransitioning)
        XCTAssertFalse(session("a", state: "exited").isTransitioning)
    }

    func testIdleTimeShowsOnRunningRowsOnly() {
        XCTAssertEqual(session("a", state: "running", idleMs: 42_000).idleLabel, "42s")
        XCTAssertNil(session("a", state: "running").idleLabel)
        XCTAssertNil(session("a", state: "exited", idleMs: 42_000).idleLabel)
        XCTAssertNil(session("a", state: "creating", idleMs: 42_000).idleLabel)
    }

    func testIdleTimeIsReadAtAGlance() {
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: 0), "0s")
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: 59_999), "59s")
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: 60_000), "1m")
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: 3_600_000), "1h")
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: 90_000_000), "1d")
        XCTAssertEqual(SessionSummary.idleLabel(milliseconds: -5), "0s")
    }

    func testTheStatusLineCoversOnlyStatesThatKeepTheList() {
        XCTAssertEqual(SessionListLinkStatus(.interrupted(.connecting(attempt: 1), nil))?.label, "Reconnecting…")
        XCTAssertEqual(SessionListLinkStatus(.macOffline(nil))?.label, "Mac unavailable")
        XCTAssertEqual(SessionListLinkStatus.connected.label, "Connected")
        XCTAssertNil(SessionListLinkStatus.connected.accessibilityDetail)
        XCTAssertNil(SessionListLinkStatus(.unlinked))
        XCTAssertNil(SessionListLinkStatus(.connecting))
        XCTAssertNil(SessionListLinkStatus(.revoked("gone")))
        XCTAssertNil(SessionListLinkStatus(.pairingRequired("again")))
        XCTAssertNil(SessionListLinkStatus(.failed("no")))
    }
}
