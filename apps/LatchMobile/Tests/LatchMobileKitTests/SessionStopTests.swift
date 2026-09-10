import XCTest

@testable import LatchMobileKit

/// Stopping a session from the phone.
///
/// The route ends a real process on someone's Mac, so the tests here are as
/// much about what the app refuses to send as about what it sends: an
/// undiscovered route is never probed, and a phone without control never
/// reaches the network at all.
@MainActor
final class SessionStopTests: XCTestCase {
    override func setUp() {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities())
        StubProtocol.stub(path: "/v2/sessions", body: Self.list(state: "running"))
        StubProtocol.stub(
            method: "POST",
            path: "/v2/sessions/ses_1/stop",
            body: #"{"id":"ses_1","state":"exited","stopped":true}"#
        )
    }

    func testStoppingPostsToTheSessionsStopRouteAndDecodesTheReport() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        let report = try await gateway.stopSession(sessionID: "ses_1")

        XCTAssertEqual(report, SessionStopReport(id: "ses_1", state: "exited", stopped: true))
        let request = try XCTUnwrap(StubProtocol.requests.last)
        XCTAssertEqual(request.method, "POST")
        XCTAssertEqual(request.path, "/v2/sessions/ses_1/stop")
        XCTAssertEqual(request.headers["Authorization"], "Bearer token")
        XCTAssertEqual(request.body, "", "a stop carries no caller-chosen options")
    }

    /// A Mac that predates the route is not probed for it. The app asks for
    /// nothing discovery did not offer.
    func testAnUndiscoveredStopRouteIsNeverProbed() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(stop: false))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        do {
            _ = try await gateway.stopSession(sessionID: "ses_1")
            XCTFail("stopping should be gated by discovery")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.stopSession))
        }
        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities"])
    }

    func testAStopEndsTheSessionAndRereadsTheListAsExited() async throws {
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        await model.refreshSessions()
        XCTAssertEqual(model.sessions.first?.state, "running")
        StubProtocol.stub(path: "/v2/sessions", body: Self.list(state: "exited"))

        let stopped = await model.stopSession(try XCTUnwrap(model.sessions.first))

        XCTAssertTrue(stopped)
        XCTAssertNil(model.sessionsError)
        // Stopping is not removing: the row stays and turns exited.
        XCTAssertEqual(model.sessions.count, 1)
        XCTAssertEqual(model.sessions.first?.state, "exited")
        XCTAssertTrue(model.stoppingSessionIDs.isEmpty)
        XCTAssertEqual(
            StubProtocol.requests.filter { $0.path == "/v2/sessions/ses_1/stop" }.count,
            1
        )
    }

    /// The control is shown on a Mac that serves the route, so a phone that
    /// lost control has to be told what to change rather than left with a
    /// button that does nothing.
    func testAGrantDowngradeKeepsTheControlVisibleAndCarriesAnExplanation() async throws {
        let model = makeModel()
        let control = pairedRecord(permission: .control)
        await model.connectPairedDevice(control)

        XCTAssertTrue(model.advertisesSessionStop)
        XCTAssertTrue(model.canStopSessions)
        XCTAssertNil(model.sessionStopUnavailableExplanation)

        XCTAssertTrue(model.applyPairedDeviceRecord(control.updating(permission: .interact)))

        XCTAssertTrue(model.advertisesSessionStop)
        XCTAssertFalse(model.canStopSessions)
        XCTAssertNotNil(model.sessionStopUnavailableExplanation)
    }

    /// Interact is enough to talk to a session and not enough to end it. The
    /// refusal is local: nothing is sent for the Mac to reject.
    func testAPhoneWithoutControlNeverSendsAStop() async throws {
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord(permission: .interact))
        await model.refreshSessions()
        let session = try XCTUnwrap(model.sessions.first)
        let requestsBefore = StubProtocol.requests.count

        let stopped = await model.stopSession(session)

        XCTAssertFalse(stopped)
        XCTAssertEqual(StubProtocol.requests.count, requestsBefore)
        XCTAssertNotNil(model.sessionsError)
    }

    /// A Mac too old to serve the route advertises nothing, so there is no
    /// control to show and nothing to explain.
    func testAPreFeatureGatewayAdvertisesNoStopAndOffersNoExplanation() async {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(stop: false))
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))

        XCTAssertFalse(model.advertisesSessionStop)
        XCTAssertFalse(model.canStopSessions)
        XCTAssertNil(model.sessionStopUnavailableExplanation)
    }

    /// The Mac waits out its own grace period before answering, so a failed
    /// stop must leave nothing marked in flight and must re-read the list —
    /// the stop may have landed even though the answer did not come back.
    func testAFailedStopClearsTheInFlightMarkAndStillRereadsTheList() async throws {
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        await model.refreshSessions()
        StubProtocol.stub(
            method: "POST",
            path: "/v2/sessions/ses_1/stop",
            status: 409,
            body: #"{"error":"session_still_running","reason":"the session did not stop"}"#
        )
        let listsBefore = StubProtocol.requests.filter { $0.path == "/v2/sessions" }.count

        let stopped = await model.stopSession(try XCTUnwrap(model.sessions.first))

        XCTAssertFalse(stopped)
        XCTAssertTrue(model.stoppingSessionIDs.isEmpty)
        XCTAssertEqual(model.sessionsError, "/v2/sessions/ses_1/stop failed (409): the session did not stop")
        XCTAssertEqual(
            StubProtocol.requests.filter { $0.path == "/v2/sessions" }.count,
            listsBefore + 1
        )
    }

    private func makeModel() -> AppModel {
        AppModel(
            pairedGatewayFactory: { _ in
                LatchGateway(
                    link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
                    session: StubProtocol.session()
                )
            },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
    }

    private func pairedRecord(permission: DevicePermission) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(
                deviceId: "mac",
                publicKey: String(repeating: "22", count: 32),
                name: "Mac"
            ),
            permission: permission,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        )
    }

    private static func capabilities(stop: Bool = true) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true,"stopSession":\(stop)},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }

    private static func list(state: String) -> String {
        """
        {"sessions":[{"id":"ses_1","name":"shell","state":"\(state)","cwd":"/Users/person/work",
         "command_label":"zsh","created_at":"2026-09-06T00:00:00Z"}]}
        """
    }
}
