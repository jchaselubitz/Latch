import XCTest

@testable import LatchMobileKit

/// Checks the client against a real `latch serve`, rather than a stub.
///
/// Skipped unless both variables are set, so the suite still runs anywhere:
///
/// ```bash
/// latch serve &
/// LATCH_GATEWAY_URL=http://127.0.0.1:4610 \
///   LATCH_GATEWAY_TOKEN="$(cat ~/.latch/serve.token)" swift test
/// ```
///
/// These assert contract behavior that holds for any conformant gateway, not
/// the contents of one machine's sessions.
final class LiveGatewayTests: XCTestCase {
    private func liveGateway() throws -> LatchGateway {
        let environment = ProcessInfo.processInfo.environment
        let url = try XCTUnwrap(
            environment["LATCH_GATEWAY_URL"],
            "set LATCH_GATEWAY_URL to run the live checks"
        )
        let token = try XCTUnwrap(environment["LATCH_GATEWAY_TOKEN"])
        return LatchGateway(link: try GatewayLink(address: url, token: token))
    }

    private func skipUnlessConfigured() throws {
        let environment = ProcessInfo.processInfo.environment
        try XCTSkipUnless(
            environment["LATCH_GATEWAY_URL"] != nil
                && environment["LATCH_GATEWAY_TOKEN"] != nil,
            "no live gateway configured"
        )
    }

    func testDiscoveryAgainstARealGateway() async throws {
        try skipUnlessConfigured()
        let capabilities = try await liveGateway().discover()
        XCTAssertEqual(
            capabilities.protocolVersion,
            LatchContract.protocolVersion,
            "this build only speaks protocol \(LatchContract.protocolVersion)"
        )
        // Sessions is the one endpoint every gateway has had since before
        // discovery existed, so it holds for a legacy gateway too.
        XCTAssertTrue(capabilities.endpoints.sessions)
    }

    func testListingRealSessions() async throws {
        try skipUnlessConfigured()
        let sessions = try await liveGateway().listSessions()
        for session in sessions {
            XCTAssertFalse(session.id.isEmpty)
            XCTAssertFalse(session.state.isEmpty)
            XCTAssertFalse(session.displayName.isEmpty)
        }
    }

    func testRealSessionCapabilitiesWhenTheGatewayOffersThem() async throws {
        try skipUnlessConfigured()
        let gateway = try liveGateway()
        let capabilities = try await gateway.discover()
        guard capabilities.endpoints.sessionCapabilities else {
            throw XCTSkip("this gateway predates per-session capabilities")
        }
        guard let session = try await gateway.listSessions().first(where: \.isRunning) else {
            throw XCTSkip("no running session to inspect")
        }
        let reported = try await gateway.sessionCapabilities(sessionId: session.id)
        // `canSend.ok == false` is a perfectly normal answer; what matters is
        // that the document decodes and carries a reason when it says no.
        if !reported.interaction.canSend.ok {
            XCTAssertNotNil(reported.interaction.canSend.reason)
        }
    }

    func testRealEventStreamReplaysFromTheCursor() async throws {
        try skipUnlessConfigured()
        let gateway = try liveGateway()
        let capabilities = try await gateway.discover()
        guard capabilities.endpoints.events else {
            throw XCTSkip("this gateway does not serve events")
        }
        guard let session = try await gateway.listSessions().first(where: \.isRunning) else {
            throw XCTSkip("no running session to watch")
        }
        let stream = try await gateway.events(sessionId: session.id)
        defer { stream.close() }

        // Take whatever the replay produces within a short window. An idle
        // session may legitimately send nothing at all, so the consumer is
        // raced against a timer rather than relying on a message arriving to
        // check the deadline — otherwise a quiet session hangs the test.
        let watch = Task { () -> (open: Bool, closed: String?) in
            var sawOpen = false
            var decoded = 0
            for await message in stream.messages {
                switch message {
                case .state(let state) where state == .open:
                    sawOpen = true
                case .event:
                    decoded += 1
                case .closed(let code, let reason):
                    return (sawOpen, "\(code) \(reason)")
                default:
                    break
                }
                if sawOpen && decoded > 0 { break }
            }
            return (sawOpen, nil)
        }
        let timeout = Task {
            try? await Task.sleep(for: .seconds(10))
            watch.cancel()
            stream.close()
        }
        let outcome = await watch.value
        timeout.cancel()
        XCTAssertNil(outcome.closed, "the stream closed: \(outcome.closed ?? "")")
        XCTAssertTrue(outcome.open, "the event socket never opened")
    }

    /// The path the Sessions tab actually takes: link, discover, list.
    @MainActor
    func testLinkingDrivesTheSessionsTab() async throws {
        try skipUnlessConfigured()
        let environment = ProcessInfo.processInfo.environment
        let model = AppModel(storage: MemoryLinkStorage())
        XCTAssertEqual(model.linkState, .unlinked)

        await model.link(
            address: try XCTUnwrap(environment["LATCH_GATEWAY_URL"]),
            token: try XCTUnwrap(environment["LATCH_GATEWAY_TOKEN"])
        )
        guard case .linked = model.linkState else {
            return XCTFail("linking failed: \(model.linkState)")
        }
        XCTAssertNil(model.sessionsError)
        XCTAssertFalse(model.sessions.isEmpty, "a live gateway should list its sessions")
        // The surface is what the views read to decide which controls exist.
        XCTAssertTrue(model.surface.chat)
    }

    /// The path the chat screen takes: subscribe, fold events into rows.
    @MainActor
    func testChatModelBuildsATranscriptFromALiveSession() async throws {
        try skipUnlessConfigured()
        let environment = ProcessInfo.processInfo.environment
        let model = AppModel(storage: MemoryLinkStorage())
        await model.link(
            address: try XCTUnwrap(environment["LATCH_GATEWAY_URL"]),
            token: try XCTUnwrap(environment["LATCH_GATEWAY_TOKEN"])
        )
        guard case .linked = model.linkState else {
            throw XCTSkip("could not link to the gateway")
        }
        guard let session = model.sessions.first(where: \.isRunning),
              let gateway = model.gateway
        else {
            throw XCTSkip("no running session to watch")
        }

        let chat = ChatModel(session: session, gateway: gateway, surface: model.surface)
        await chat.start()
        defer { chat.stop() }

        // Give the replay a bounded moment to arrive. A session with no harness
        // history legitimately produces nothing, so an empty transcript is not
        // a failure — decoding without error is what is being checked.
        for _ in 0..<40 where chat.transcript.entries.isEmpty && chat.endedReason == nil {
            try await Task.sleep(for: .milliseconds(250))
        }
        XCTAssertNil(chat.error, "the chat screen reported: \(chat.error ?? "")")
        if let ended = chat.endedReason {
            // A session with no connector closes 4408, which is a legitimate
            // outcome the screen explains rather than an error.
            XCTAssertTrue(
                ended.contains("no agent attached"),
                "unexpected stream end: \(ended)"
            )
        }
    }
}
