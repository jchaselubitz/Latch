import XCTest

@testable import LatchMobileKit

/// `latch://sessions/<id>` is how Overlord opens a mission's live session.
/// The link names a session and nothing else, so parsing is strict: anything
/// that is not exactly one well-formed id is refused rather than looked up.
@MainActor
final class SessionDeepLinkTests: XCTestCase {
    override func setUp() {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: Self.list(ids: ["ses_1"]))
    }

    func testParsesASessionLink() throws {
        let url = try XCTUnwrap(URL(string: "latch://sessions/ses_1a0d7323104aa9e0"))
        XCTAssertEqual(SessionDeepLink.sessionID(from: url), "ses_1a0d7323104aa9e0")
    }

    func testBuildsTheLinkItParses() throws {
        let url = try XCTUnwrap(SessionDeepLink.url(forSession: "ses_abc-123"))
        XCTAssertEqual(url.absoluteString, "latch://sessions/ses_abc-123")
        XCTAssertEqual(SessionDeepLink.sessionID(from: url), "ses_abc-123")
    }

    func testRefusesAnythingThatIsNotExactlyOneSessionID() {
        let refused = [
            "https://sessions/ses_1",
            "latch://session/ses_1",
            "latch://sessions",
            "latch://sessions/",
            "latch://sessions/ses_1/terminal",
            "latch://sessions/ses%201",
            "latch://sessions/\(String(repeating: "a", count: 129))",
        ]
        for string in refused {
            guard let url = URL(string: string) else { continue }
            XCTAssertNil(SessionDeepLink.sessionID(from: url), string)
        }
        XCTAssertNil(SessionDeepLink.url(forSession: ""))
        XCTAssertNil(SessionDeepLink.url(forSession: "../ses_1"))
    }

    func testALinkBeforeTheRouteIsUpWaitsForTheFirstListing() async throws {
        let model = makeModel()
        await model.requestSession(id: "ses_1")
        XCTAssertEqual(model.requestedSessionID, "ses_1")
        XCTAssertNil(model.takeRequestedSession())

        await model.connectPairedDevice(pairedRecord)

        XCTAssertEqual(model.takeRequestedSession()?.id, "ses_1")
        XCTAssertNil(model.requestedSessionID)
        XCTAssertNil(model.requestedSessionError)
    }

    func testALinkOnALiveRouteRereadsTheListForANewSession() async throws {
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord)
        StubProtocol.stub(path: "/v2/sessions", body: Self.list(ids: ["ses_1", "ses_2"]))

        await model.requestSession(id: "ses_2")

        XCTAssertEqual(model.takeRequestedSession()?.id, "ses_2")
    }

    func testALinkToASessionTheMacDoesNotHaveExplainsItself() async throws {
        let model = makeModel()
        await model.connectPairedDevice(pairedRecord)

        await model.requestSession(id: "ses_gone")

        XCTAssertNil(model.requestedSessionID)
        XCTAssertNil(model.takeRequestedSession())
        let error = try XCTUnwrap(model.requestedSessionError)
        XCTAssertTrue(error.contains("ses_gone"))
        XCTAssertTrue(error.contains("Mac"))
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

    private var pairedRecord: PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(
                deviceId: "mac",
                publicKey: String(repeating: "22", count: 32),
                name: "Mac"
            ),
            permission: .control,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        )
    }

    private static let capabilities = """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true,"stopSession":true},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """

    private static func list(ids: [String]) -> String {
        let rows = ids.map {
            """
            {"id":"\($0)","name":"shell","state":"running","cwd":"/Users/person/work",
             "command_label":"zsh","created_at":"2026-09-06T00:00:00Z"}
            """
        }
        return #"{"sessions":["# + rows.joined(separator: ",") + "]}"
    }
}
