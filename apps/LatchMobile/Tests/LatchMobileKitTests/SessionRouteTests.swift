import XCTest

@testable import LatchMobileKit

/// Where a tap lands is decided by four answers, not one: the preference, the
/// session's connector, what the gateway and this device's grant permit, and
/// whether there is anything running to attach to. The resolution is a pure
/// function precisely so the table can be asserted here rather than driven
/// through navigation.
final class SessionRouteTests: XCTestCase {
    private func surface(chat: Bool, terminal: Bool, terminalAdvertised: Bool? = nil) -> SessionSurface {
        SessionSurface(
            chat: chat,
            composer: chat,
            interactionControls: chat,
            terminal: terminal,
            terminalAdvertised: terminalAdvertised
        )
    }

    func testTheResolutionTable() {
        let cases: [(
            line: UInt,
            preference: SessionPresentation,
            connector: SessionConnector,
            surface: SessionSurface,
            isRunning: Bool,
            expected: SessionRoute
        )] = [
            // Terminal preferred, terminal available: the terminal, and a
            // running session is taken on arrival.
            (#line, .terminal, .none, surface(chat: true, terminal: true), true, .terminal(autoAttach: true)),
            (#line, .terminal, .named("claude"), surface(chat: true, terminal: true), true, .terminal(autoAttach: true)),
            (#line, .terminal, .unknown, surface(chat: false, terminal: true), true, .terminal(autoAttach: true)),
            // Nothing to steal from a session that is not running.
            (#line, .terminal, .named("claude"), surface(chat: true, terminal: true), false, .terminal(autoAttach: false)),

            // Terminal preferred but impossible: chat where chat can work.
            (#line, .terminal, .named("codex"), surface(chat: true, terminal: false), true, .chat),
            (#line, .terminal, .unknown, surface(chat: true, terminal: false), true, .chat),

            // Terminal preferred, no connector, no terminal. Which explanation
            // depends on whether the gateway offered the route at all.
            (
                #line, .terminal, .none,
                surface(chat: true, terminal: false, terminalAdvertised: true), true,
                .unavailable(.needsControlGrant)
            ),
            (
                #line, .terminal, .none,
                surface(chat: true, terminal: false, terminalAdvertised: false), true,
                .unavailable(.noTerminalEndpoint)
            ),
            (
                #line, .terminal, .none,
                surface(chat: false, terminal: false, terminalAdvertised: false), true,
                .unavailable(.noConversation)
            ),

            // Chat preferred, and chat is possible.
            (#line, .chat, .named("claude"), surface(chat: true, terminal: true), true, .chat),
            // An older Mac omits the field. It must keep behaving as it does
            // today, including ChatView's own "connector is null" screen.
            (#line, .chat, .unknown, surface(chat: true, terminal: true), true, .chat),

            // Chat preferred, no connector: the terminal, but the steal is not
            // implied by a tap that asked for something else.
            (#line, .chat, .none, surface(chat: true, terminal: true), true, .terminal(autoAttach: false)),
            (
                #line, .chat, .none,
                surface(chat: true, terminal: false, terminalAdvertised: true), true,
                .unavailable(.needsControlGrant)
            ),
            // No Hub either: chat is not possible for any session here.
            (#line, .chat, .named("claude"), surface(chat: false, terminal: true), true, .terminal(autoAttach: false)),
        ]

        for row in cases {
            XCTAssertEqual(
                SessionRoute.route(
                    preference: row.preference,
                    connector: row.connector,
                    surface: row.surface,
                    isRunning: row.isRunning
                ),
                row.expected,
                "preference \(row.preference), connector \(row.connector)",
                line: row.line
            )
        }
    }

    // MARK: - The grant gate

    func testAnObserveOrInteractPhoneResolvesNoTerminal() {
        let advertised = GatewayCompatibility.sessionSurface(for: capabilities(terminal: true))
        XCTAssertTrue(advertised.terminal)
        XCTAssertFalse(advertised.restricted(to: .observe).terminal)
        XCTAssertFalse(advertised.restricted(to: .interact).terminal)
    }

    func testAControlPhoneAndAManualLinkResolveATerminal() {
        let advertised = GatewayCompatibility.sessionSurface(for: capabilities(terminal: true))
        XCTAssertTrue(advertised.restricted(to: .control).terminal)
        // A manual `latch serve` link sends no grant header, and http.rs
        // grants loopback requests Grant::Control.
        XCTAssertTrue(advertised.restricted(to: nil).terminal)
    }

    /// The refusal screens have to tell a grant apart from an old Mac, so the
    /// gateway's own answer survives the restriction that clears `terminal`.
    func testTheGatewaysOfferSurvivesTheGrantRestriction() {
        let advertised = GatewayCompatibility.sessionSurface(for: capabilities(terminal: true))
        XCTAssertTrue(advertised.restricted(to: .observe).terminalAdvertised)

        let notOffered = GatewayCompatibility.sessionSurface(for: capabilities(terminal: false))
        XCTAssertFalse(notOffered.terminalAdvertised)
        XCTAssertFalse(notOffered.restricted(to: .control).terminalAdvertised)
    }

    // MARK: - Storage

    func testThePreferenceDefaultsToTerminalAndSurvivesASave() {
        let defaults = UserDefaults(suiteName: "SessionRouteTests.\(UUID().uuidString)")!
        let store = UserDefaultsSessionPresentationStore(defaults: defaults)
        XCTAssertEqual(store.load(), .terminal)
        store.save(.chat)
        XCTAssertEqual(UserDefaultsSessionPresentationStore(defaults: defaults).load(), .chat)
    }

    func testAnUnrecognisedStoredValueFallsBackToTheDefault() {
        let defaults = UserDefaults(suiteName: "SessionRouteTests.\(UUID().uuidString)")!
        defaults.set("hologram", forKey: "sessionPresentation")
        XCTAssertEqual(UserDefaultsSessionPresentationStore(defaults: defaults).load(), .terminal)
    }

    func testTheTerminalSizeDefaultsToMatchingTheMac() {
        let store = MemoryTerminalSizeStore()
        XCTAssertEqual(store.load(), .matchMac)
        XCTAssertNil(TerminalSize.matchMac.fixedGrid)
        XCTAssertEqual(TerminalSize.fixed80x24.fixedGrid?.cols, 80)
        XCTAssertEqual(TerminalSize.fixed100x30.fixedGrid?.rows, 30)
    }

    private func capabilities(terminal: Bool) -> GatewayCapabilities {
        let json = """
        {"protocolVersion":\(LatchContract.protocolVersion),"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":\(terminal),"conversation":true},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
        // swiftlint:disable:next force_try
        return try! JSONDecoder().decode(GatewayCapabilities.self, from: Data(json.utf8))
    }
}
