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
