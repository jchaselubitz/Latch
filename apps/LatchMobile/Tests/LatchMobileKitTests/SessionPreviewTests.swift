import XCTest

@testable import LatchMobileKit

/// `GET /v2/sessions/{id}/preview` — the read of the live pane that does not
/// attach, and therefore does not steal.
final class SessionPreviewTests: XCTestCase {
    override func setUp() { StubProtocol.reset() }

    private static func capabilities(preview: String) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,\(preview)"terminal":true,"conversation":true},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }

    private func gateway() -> LatchGateway {
        LatchGateway(
            link: try! GatewayLink(address: "http://127.0.0.1:8787", token: "token"),
            session: StubProtocol.session()
        )
    }

    func testPreviewDecodesTheCapturedPane() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(preview: #""preview":true,"#))
        StubProtocol.stub(path: "/v2/sessions/ses_1/preview", body: """
        {"content":"\\u001b[32mgreen\\u001b[0m","cols":100,"rows":30,
         "alternateScreen":true,"capturedAt":"2026-08-24T09:41:02Z","scrollbackLines":0}
        """)

        let preview = try await gateway().previewSession(sessionID: "ses_1")

        XCTAssertEqual(preview.content, "\u{1b}[32mgreen\u{1b}[0m")
        // The desk's own grid. Phase 4 attaches at exactly this size so the
        // pane never resizes and a paused prompt transfers as it stands.
        XCTAssertEqual(preview.cols, 100)
        XCTAssertEqual(preview.rows, 30)
        XCTAssertTrue(preview.alternateScreen)
        XCTAssertEqual(preview.capturedAt, "2026-08-24T09:41:02Z")
        XCTAssertEqual(preview.scrollbackLines, 0)
    }

    /// Zero is the common case and must not put an empty parameter on the URL;
    /// a real request carries the number the caller asked for.
    func testScrollbackIsOnTheQueryOnlyWhenAskedFor() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(preview: #""preview":true,"#))
        StubProtocol.stub(path: "/v2/sessions/ses_1/preview", body: """
        {"content":"","cols":80,"rows":24,"alternateScreen":false,
         "capturedAt":"2026-08-24T09:41:02Z","scrollbackLines":40}
        """)

        _ = try await gateway().previewSession(sessionID: "ses_1")
        _ = try await gateway().previewSession(sessionID: "ses_1", scrollbackLines: 40)

        let queries = StubProtocol.requestQueries.filter { $0.0.hasSuffix("/preview") }
        XCTAssertEqual(queries.map(\.1), [nil, "scrollbackLines=40"])
    }

    /// A Mac that predates the route omits the key entirely. That must read as
    /// "unavailable" without failing the whole discovery document, and the app
    /// must not send the request anyway to find out.
    func testAnOlderGatewayReportsPreviewUnavailableWithoutBeingProbed() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(preview: ""))

        do {
            _ = try await gateway().previewSession(sessionID: "ses_1")
            XCTFail("preview should be refused when discovery does not advertise it")
        } catch let error as LatchError {
            XCTAssertEqual(error, .endpointUnavailable(.preview))
        }

        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities"])
    }

    func testAbsentPreviewKeyDecodesAsFalseRatherThanFailing() throws {
        let endpoints = try JSONDecoder().decode(
            GatewayEndpoints.self,
            from: Data(#"{"sessions":true,"terminal":true,"conversation":true}"#.utf8)
        )
        XCTAssertFalse(endpoints.preview)
        XCTAssertTrue(endpoints.sessions)
    }
}
