import XCTest

@testable import LatchMobileKit

/// `openTerminal` against the discovery stub. The socket itself is never
/// opened here — what is under test is the gate and the upgrade URL.
final class TerminalGatewayTests: XCTestCase {
    override func setUp() { StubProtocol.reset() }

    private func capabilities(terminal: Bool) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"terminal":\(terminal),"conversation":true},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }

    func testTheGridTravelsOnTheUpgradeURLRatherThanAHandshakeFrame() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities(terminal: true))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        let connection = try await gateway.openTerminal(sessionID: "ses_a", cols: 100, rows: 30)
        defer { connection.cancel() }

        let url = try XCTUnwrap((connection as? URLSessionTerminalSocketConnection)?.url)
        XCTAssertEqual(url.scheme, "wss")
        XCTAssertEqual(url.path, "/v2/sessions/ses_a/terminal")
        let query = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(query.first { $0.name == "cols" }?.value, "100")
        XCTAssertEqual(query.first { $0.name == "rows" }?.value, "30")
    }

    func testAGatewayWithoutTheTerminalEndpointIsRefusedBeforeAnySocketOpens() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities(terminal: false))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        do {
            _ = try await gateway.openTerminal(sessionID: "ses_a", cols: 80, rows: 24)
            XCTFail("a gateway that does not advertise a terminal must not be dialled")
        } catch let error as LatchError {
            XCTAssertEqual(error, .endpointUnavailable(.terminal))
        }
    }

    func testATerminalIsAControlSurfaceAndObserveOrInteractMayNotOpenOne() {
        let surface = SessionSurface(chat: true, composer: true, interactionControls: true, terminal: true)
        XCTAssertFalse(surface.restricted(to: .observe).terminal)
        XCTAssertFalse(surface.restricted(to: .interact).terminal)
        XCTAssertTrue(surface.restricted(to: .control).terminal)
        // A manual `latch serve` link carries no grant at all, and the gateway
        // grants loopback requests control.
        XCTAssertTrue(surface.restricted(to: nil).terminal)
    }
}
