import XCTest

@testable import LatchMobileKit

/// A stub HTTP transport, so the client's contract behavior is testable
/// without a running `latch serve`.
final class StubProtocol: URLProtocol {
    struct Reply {
        var status: Int
        var body: String
        var json: Bool = true
    }

    private static let lock = NSLock()
    nonisolated(unsafe) private static var replies: [String: Reply] = [:]
    nonisolated(unsafe) private static var seen: [(path: String, headers: [String: String], body: String)] = []

    static func stub(path: String, status: Int = 200, body: String) {
        lock.lock()
        defer { lock.unlock() }
        replies[path] = Reply(status: status, body: body)
    }

    static func reset() {
        lock.lock()
        defer { lock.unlock() }
        replies = [:]
        seen = []
    }

    static var requests: [(path: String, headers: [String: String], body: String)] {
        lock.lock()
        defer { lock.unlock() }
        return seen
    }

    static func session() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        return URLSession(configuration: configuration)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let path = request.url?.path ?? ""
        // `httpBody` is stripped from a URLProtocol's request, so the stream
        // is what carries a POST body here.
        var body = ""
        if let stream = request.httpBodyStream {
            stream.open()
            var data = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let read = stream.read(&buffer, maxLength: buffer.count)
                if read <= 0 { break }
                data.append(contentsOf: buffer[0..<read])
            }
            stream.close()
            body = String(data: data, encoding: .utf8) ?? ""
        }
        Self.lock.lock()
        Self.seen.append((path, request.allHTTPHeaderFields ?? [:], body))
        let reply = Self.replies[path]
        Self.lock.unlock()

        let found = reply ?? Reply(status: 404, body: #"{"error":"not found"}"#)
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: found.status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(found.body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

final class GatewayTests: XCTestCase {
    private let fullDiscovery = """
    {"protocolVersion":1,"productVersion":"1.0.0",
     "endpoints":{"sessions":true,"sessionCapabilities":true,"terminal":true,
                  "events":true,"send":true},
     "features":{"idempotencyKeys":true,"readOnlyTerminal":true},
     "gatewayInstanceId":"gw-a-b"}
    """

    override func setUp() {
        super.setUp()
        StubProtocol.reset()
    }

    private func gateway() throws -> LatchGateway {
        LatchGateway(
            link: try GatewayLink(address: "http://127.0.0.1:8787", token: "t0ken"),
            session: StubProtocol.session()
        )
    }

    func testLinkRejectsAnAddressItCannotUse() {
        XCTAssertThrowsError(try GatewayLink(address: "not a url", token: "t"))
        XCTAssertThrowsError(try GatewayLink(address: "ftp://host", token: "t"))
        XCTAssertThrowsError(try GatewayLink(address: "", token: "t"))
    }

    func testLinkNormalizesATrailingSlash() throws {
        let link = try GatewayLink(address: "  https://tunnel.example/ ", token: " tok ")
        XCTAssertEqual(link.url.absoluteString, "https://tunnel.example")
        XCTAssertEqual(link.token, "tok")
    }

    func testDiscoveryIsSentWithTheBearerToken() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        let gateway = try gateway()
        let capabilities = try await gateway.discover()
        XCTAssertEqual(capabilities.gatewayInstanceId, "gw-a-b")
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.headers["Authorization"], "Bearer t0ken")
    }

    func testAnEmptyTunnelTokenDoesNotProduceAnAuthorizationHeader() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        let link = try GatewayLink(address: "http://127.0.0.1:8787", token: "")
        let gateway = LatchGateway(transport: HTTPSGatewayTransport(link: link), session: StubProtocol.session())
        _ = try await gateway.discover()
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertNil(request.headers["Authorization"])
    }

    func testA404OnDiscoveryIsTheLegacyGatewayNotAFailure() async throws {
        StubProtocol.stub(path: "/v1/capabilities", status: 404, body: "not found")
        let capabilities = try await gateway().discover()
        XCTAssertTrue(capabilities.endpoints.sessions)
        XCTAssertFalse(capabilities.endpoints.events)
    }

    func testAControlPlane404OnDiscoveryIsNotALegacyGateway() async throws {
        StubProtocol.stub(
            path: "/v1/capabilities",
            status: 404,
            body: #"{"error":"not_found","reason":"no such resource"}"#
        )
        do {
            _ = try await gateway().discover()
            XCTFail("the control plane must not be treated as a pre-discovery gateway")
        } catch let error as LatchError {
            XCTAssertEqual(error, .notAGateway)
        }
    }

    func testListingSessionsAgainstTheControlPlaneNamesTheWrongService() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions",
            status: 404,
            body: #"{"error":"not_found","reason":"no such resource"}"#
        )
        do {
            _ = try await gateway().listSessions()
            XCTFail("a control-plane 404 must not be reported as a missing session route")
        } catch let error as LatchError {
            XCTAssertEqual(error, .notAGateway)
        }
    }

    func testAnUnsupportedProtocolMajorIsRefused() async throws {
        StubProtocol.stub(
            path: "/v1/capabilities",
            body: #"{"protocolVersion":2,"productVersion":"9","gatewayInstanceId":"gw-9-9"}"#
        )
        do {
            _ = try await gateway().discover()
            XCTFail("a protocol major this build does not implement must be refused")
        } catch let error as LatchError {
            XCTAssertEqual(error, .unsupportedProtocol(reported: 2, supported: 1))
        }
    }

    func testAnUndiscoveredEndpointIsNeverProbed() async throws {
        // The whole point of discovery: a gateway without `send` gets no
        // request at all, rather than a POST whose error we interpret.
        StubProtocol.stub(path: "/v1/capabilities", status: 404, body: "")
        let gateway = try gateway()
        do {
            _ = try await gateway.send(sessionId: "s", operation: .message("hi"))
            XCTFail("send must not be attempted against a gateway that lacks it")
        } catch let error as LatchError {
            XCTAssertEqual(error, .endpointUnavailable(.send))
        }
        XCTAssertEqual(
            StubProtocol.requests.map(\.path),
            ["/v1/capabilities"],
            "no request may be sent to the undiscovered endpoint"
        )
    }

    func testSendCarriesAnIdempotencyKeyForAMessage() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions/ses_1/send",
            body: #"{"sessionId":"ses_1","operation":"message","sent":true,"resolved":false}"#
        )
        let report = try await gateway().send(sessionId: "ses_1", operation: .message("hello"))
        XCTAssertTrue(report.sent)
        let send = try XCTUnwrap(
            StubProtocol.requests.first { $0.path == "/v1/sessions/ses_1/send" }
        )
        XCTAssertNotNil(send.headers[LatchContract.idempotencyKeyHeader])
        XCTAssertEqual(send.body, #"{"message":"hello"}"#)
    }

    func testARetryReusesTheSuppliedKey() async throws {
        // A retry after an ambiguous failure must present the same key, or the
        // gateway treats it as a second, different submission.
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions/ses_1/send",
            body: #"{"sessionId":"ses_1","operation":"message","sent":true,"resolved":false}"#
        )
        let gateway = try gateway()
        _ = try await gateway.send(
            sessionId: "ses_1",
            operation: .message("hi"),
            idempotencyKey: "fixed-key"
        )
        _ = try await gateway.send(
            sessionId: "ses_1",
            operation: .message("hi"),
            idempotencyKey: "fixed-key"
        )
        let keys = StubProtocol.requests
            .filter { $0.path == "/v1/sessions/ses_1/send" }
            .compactMap { $0.headers[LatchContract.idempotencyKeyHeader] }
        XCTAssertEqual(keys, ["fixed-key", "fixed-key"])
    }

    func testKeysNeverCarryAnIdempotencyKey() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions/ses_1/send",
            body: #"{"sessionId":"ses_1","operation":"keys","sent":true,"resolved":false}"#
        )
        _ = try await gateway().send(sessionId: "ses_1", operation: .keys("C-c"))
        let send = try XCTUnwrap(
            StubProtocol.requests.first { $0.path == "/v1/sessions/ses_1/send" }
        )
        XCTAssertNil(
            send.headers[LatchContract.idempotencyKeyHeader],
            "the gateway rejects the header on a non-idempotent operation"
        )
    }

    func testNoKeyIsSentWhenTheFeatureWasNotDiscovered() async throws {
        StubProtocol.stub(
            path: "/v1/capabilities",
            body: """
            {"protocolVersion":1,"productVersion":"0.9","gatewayInstanceId":"gw-1-1",
             "endpoints":{"sessions":true,"sessionCapabilities":true,"terminal":true,
                          "events":true,"send":true},
             "features":{"idempotencyKeys":false,"readOnlyTerminal":false}}
            """
        )
        StubProtocol.stub(
            path: "/v1/sessions/s/send",
            body: #"{"sessionId":"s","operation":"message","sent":true,"resolved":false}"#
        )
        _ = try await gateway().send(sessionId: "s", operation: .message("hi"))
        let send = try XCTUnwrap(StubProtocol.requests.first { $0.path == "/v1/sessions/s/send" })
        XCTAssertNil(send.headers[LatchContract.idempotencyKeyHeader])
    }

    func testARefusalIsDistinctFromAnAmbiguousName() async throws {
        // Both are 409. Only the first is the harness declining, and only the
        // first should be shown to the user as a reason input was rejected.
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions/s/send",
            status: 409,
            body: #"{"error":"refused","reason":"the composer already contains text"}"#
        )
        do {
            _ = try await gateway().send(sessionId: "s", operation: .message("hi"))
            XCTFail("expected a refusal")
        } catch let error as LatchError {
            XCTAssertEqual(error, .refused("the composer already contains text"))
        }

        StubProtocol.reset()
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions/s/send",
            status: 409,
            body: #"{"error":"session name is ambiguous"}"#
        )
        do {
            _ = try await gateway().send(sessionId: "s", operation: .message("hi"))
            XCTFail("expected a conflict")
        } catch let error as LatchError {
            guard case .http(let status, _, let reason) = error else {
                return XCTFail("an ambiguous name is a lookup failure, not a refusal")
            }
            XCTAssertEqual(status, 409)
            XCTAssertEqual(reason, "session name is ambiguous")
        }
    }

    func testARejectedTokenIsReportedAsSuch() async throws {
        StubProtocol.stub(path: "/v1/capabilities", status: 401, body: #"{"error":"invalid token"}"#)
        do {
            _ = try await gateway().discover()
            XCTFail("expected an authorization failure")
        } catch let error as LatchError {
            XCTAssertEqual(error, .unauthorized)
        }
    }

    func testSessionsAreListedAfterDiscovery() async throws {
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(
            path: "/v1/sessions",
            body: """
            {"sessions":[{"id":"ses_1","name":"a","state":"running","cwd":"/w/Latch",
              "command_label":"claude","created_at":"2026-08-14T10:00:00Z"}]}
            """
        )
        let sessions = try await gateway().listSessions()
        XCTAssertEqual(sessions.map(\.id), ["ses_1"])
    }

    func testTranscriptRetryPolicyHasABoundedMissingTranscriptGracePeriod() {
        let policy = RetryPolicy(transcriptNotFoundAttempts: 0)
        XCTAssertEqual(policy.transcriptNotFoundAttempts, 1)
        XCTAssertTrue(policy.missingTranscriptIsFatal(failures: 1))

        let normal = RetryPolicy()
        XCTAssertEqual(normal.transcriptNotFoundAttempts, 8)
        XCTAssertFalse(normal.missingTranscriptIsFatal(failures: 7))
        XCTAssertTrue(normal.missingTranscriptIsFatal(failures: 8))
    }

    func testInvalidatingDiscoveryForcesItToRunAgain() async throws {
        // Required after an interruption: capabilities may have changed while
        // the phone was offline, so the old answers must not be reused.
        StubProtocol.stub(path: "/v1/capabilities", body: fullDiscovery)
        StubProtocol.stub(path: "/v1/sessions", body: #"{"sessions":[]}"#)
        let gateway = try gateway()
        _ = try await gateway.listSessions()
        _ = try await gateway.listSessions()
        XCTAssertEqual(
            StubProtocol.requests.filter { $0.path == "/v1/capabilities" }.count,
            1,
            "discovery is cached between calls"
        )
        await gateway.invalidateDiscovery()
        _ = try await gateway.listSessions()
        XCTAssertEqual(
            StubProtocol.requests.filter { $0.path == "/v1/capabilities" }.count,
            2
        )
    }
}
