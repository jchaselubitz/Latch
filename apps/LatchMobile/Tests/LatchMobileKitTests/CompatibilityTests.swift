import XCTest

@testable import LatchMobileKit

/// The rules in `docs/REMOTE_SDK.md` that keep this app honest against a
/// gateway that is older or newer than it.
final class CompatibilityTests: XCTestCase {
    private func capabilities(
        protocolVersion: Int = 1,
        endpoints: GatewayEndpoints = GatewayEndpoints(
            sessions: true,
            sessionCapabilities: true,
            terminal: true,
            events: true,
            send: true
        ),
        features: GatewayFeatures = GatewayFeatures(
            idempotencyKeys: true,
            readOnlyTerminal: true
        )
    ) -> GatewayCapabilities {
        GatewayCapabilities(
            protocolVersion: protocolVersion,
            productVersion: "1.0.0",
            endpoints: endpoints,
            features: features,
            gatewayInstanceId: "gw-1-1"
        )
    }

    func testAnUndiscoveredGatewaySupportsNothing() {
        // Discovery is mandatory. Before it runs, the answer to "may I call
        // this?" is no, not "probably".
        for endpoint in GatewayEndpointsName.allCases {
            XCTAssertFalse(
                GatewayCompatibility.supports(endpoint: endpoint, capabilities: nil)
            )
        }
    }

    func testADisabledEndpointIsNeverUsed() {
        let discovered = capabilities(
            endpoints: GatewayEndpoints(
                sessions: true,
                sessionCapabilities: false,
                terminal: true,
                events: false,
                send: false
            )
        )
        XCTAssertTrue(GatewayCompatibility.supports(endpoint: .sessions, capabilities: discovered))
        XCTAssertFalse(GatewayCompatibility.supports(endpoint: .events, capabilities: discovered))
        XCTAssertFalse(GatewayCompatibility.supports(endpoint: .send, capabilities: discovered))
    }

    func testAnUnsupportedProtocolMajorDisablesEverything() {
        // Field meanings are only guaranteed within one major. Once the major
        // disagrees, "the map said true" means nothing.
        let future = capabilities(protocolVersion: 2)
        for endpoint in GatewayEndpointsName.allCases {
            XCTAssertFalse(
                GatewayCompatibility.supports(endpoint: endpoint, capabilities: future),
                "\(endpoint.rawValue) must not be used across a protocol major"
            )
        }
        for feature in GatewayFeaturesName.allCases {
            XCTAssertFalse(
                GatewayCompatibility.supports(feature: feature, capabilities: future)
            )
        }
        XCTAssertThrowsError(try GatewayCompatibility.validate(future)) { error in
            XCTAssertEqual(
                error as? LatchError,
                .unsupportedProtocol(reported: 2, supported: 1)
            )
        }
    }

    func testAProtocolMismatchNamesTheSideThatCanAct() {
        // Both directions disable everything, but only one of them is the
        // person's to fix, and they are holding exactly one of the two
        // devices. A gateway ahead of the app is the ordinary case: the CLI
        // updates itself, this app waits on the App Store.
        let behind = ProtocolMismatch(reported: 2, supported: 1)
        XCTAssertEqual(behind, .updatePhone(reported: 2, supported: 1))
        XCTAssertEqual(behind.title, "Update Latch on this phone")
        XCTAssertTrue(
            behind.detail.contains("App Store"),
            "the phone-side remedy has to name where the update comes from"
        )

        let ahead = ProtocolMismatch(reported: 1, supported: 2)
        XCTAssertEqual(ahead, .updateComputer(reported: 1, supported: 2))
        XCTAssertEqual(ahead.title, "Update Latch on your computer")
        XCTAssertTrue(ahead.detail.contains("latch update"))
    }

    func testTheMismatchExplainsWhyTheTerminalWentAwayToo() {
        // The terminal is the fallback people reach for when chat is broken.
        // `supports` disables it along with everything else across a major, so
        // its absence is explained where the mismatch is reported rather than
        // discovered on the session screen.
        for mismatch in [
            ProtocolMismatch(reported: 2, supported: 1),
            ProtocolMismatch(reported: 1, supported: 2)
        ] {
            XCTAssertTrue(
                mismatch.detail.contains("terminal"),
                "\(mismatch.title) must account for the terminal being unavailable"
            )
        }
    }

    func testTheUnsupportedProtocolErrorCarriesTheMismatch() {
        // The generic message path and the actionable screen must not drift:
        // both derive from one classification.
        let error = LatchError.unsupportedProtocol(reported: 2, supported: 1)
        XCTAssertEqual(error.protocolMismatch, .updatePhone(reported: 2, supported: 1))
        XCTAssertTrue(error.message.contains("Update Latch on this phone"))
        XCTAssertNil(LatchError.unauthorized.protocolMismatch)
        XCTAssertNil(LatchError.notAGateway.protocolMismatch)
    }

    func testTheLegacyGatewayKeepsSessionsAndTerminalOnly() {
        // A 404 on /v1/capabilities identifies the pre-discovery gateway.
        // Sessions and terminal predate discovery; everything introduced with
        // it stays off, because the only way to learn about it is a probe.
        let legacy = GatewayCompatibility.legacyCapabilities()
        XCTAssertTrue(GatewayCompatibility.supports(endpoint: .sessions, capabilities: legacy))
        XCTAssertTrue(GatewayCompatibility.supports(endpoint: .terminal, capabilities: legacy))
        XCTAssertFalse(GatewayCompatibility.supports(endpoint: .events, capabilities: legacy))
        XCTAssertFalse(GatewayCompatibility.supports(endpoint: .send, capabilities: legacy))
        XCTAssertFalse(
            GatewayCompatibility.supports(endpoint: .sessionCapabilities, capabilities: legacy)
        )
        for feature in GatewayFeaturesName.allCases {
            XCTAssertFalse(GatewayCompatibility.supports(feature: feature, capabilities: legacy))
        }
    }

    func testTheControlPlaneUnmatchedRouteIsNotALegacyGateway() {
        XCTAssertTrue(
            GatewayCompatibility.isControlPlaneUnmatchedRoute(
                status: 404,
                code: "not_found",
                reason: "no such resource"
            )
        )
        XCTAssertFalse(
            GatewayCompatibility.isControlPlaneUnmatchedRoute(
                status: 404,
                code: nil,
                reason: "not found"
            ),
            "a pre-discovery latch serve 404 must still map to the legacy surface"
        )
        XCTAssertFalse(
            GatewayCompatibility.isControlPlaneUnmatchedRoute(
                status: 404,
                code: "not_found",
                reason: "not_found"
            )
        )
    }

    func testNoEventsEndpointMeansNoChatSurfaceAtAll() {
        let terminalOnly = GatewayCompatibility.sessionSurface(
            for: GatewayCompatibility.legacyCapabilities()
        )
        XCTAssertEqual(
            terminalOnly,
            SessionSurface(chat: false, composer: false, interactionControls: false)
        )
    }

    func testEventsWithoutSendIsATranscriptOnlySurface() {
        let readOnly = capabilities(
            endpoints: GatewayEndpoints(
                sessions: true,
                sessionCapabilities: false,
                terminal: true,
                events: true,
                send: false
            )
        )
        let surface = GatewayCompatibility.sessionSurface(for: readOnly)
        XCTAssertTrue(surface.chat)
        XCTAssertFalse(surface.composer)
        XCTAssertFalse(surface.interactionControls)
    }

    func testAFullGatewayOffersEverything() {
        let surface = GatewayCompatibility.sessionSurface(for: capabilities())
        XCTAssertEqual(
            surface,
            SessionSurface(chat: true, composer: true, interactionControls: true)
        )
    }
}
