import XCTest

@testable import LatchMobileKit

final class StubProtocol: URLProtocol {
    struct Reply { var status: Int; var body: String }
    private static let lock = NSLock()
    nonisolated(unsafe) private static var replies: [String: Reply] = [:]
    nonisolated(unsafe) private static var seen: [(method: String, path: String, query: String?, headers: [String: String], body: String)] = []

    static func stub(path: String, status: Int = 200, body: String) {
        lock.withLock { replies[path] = Reply(status: status, body: body) }
    }
    static func stub(method: String, path: String, status: Int = 200, body: String) {
        lock.withLock { replies["\(method) \(path)"] = Reply(status: status, body: body) }
    }
    static func reset() { lock.withLock { replies = [:]; seen = [] } }
    static var requests: [(method: String, path: String, query: String?, headers: [String: String], body: String)] {
        lock.withLock { seen }
    }

    /// Path and raw query per request, for assertions about what ended up on
    /// the URL rather than only which route was called.
    static var requestQueries: [(String, String?)] {
        lock.withLock { seen.map { ($0.path, $0.query) } }
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
        let query = request.url.flatMap {
            URLComponents(url: $0, resolvingAgainstBaseURL: false)?.percentEncodedQuery
        }
        let body = Self.body(of: request)
        Self.lock.withLock {
            Self.seen.append((request.httpMethod ?? "GET", path, query, request.allHTTPHeaderFields ?? [:], String(decoding: body, as: UTF8.self)))
        }
        let method = request.httpMethod ?? "GET"
        let found = Self.lock.withLock { Self.replies["\(method) \(path)"] ?? Self.replies[path] }
            ?? Reply(status: 404, body: #"{"error":"not found"}"#)
        let response = HTTPURLResponse(
            url: request.url!, statusCode: found.status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(found.body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}

    private static func body(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var body = Data()
        var buffer = [UInt8](repeating: 0, count: 4_096)
        while stream.hasBytesAvailable {
            let count = stream.read(&buffer, maxLength: buffer.count)
            guard count > 0 else { break }
            body.append(buffer, count: count)
        }
        return body
    }
}

final class GatewayV2Tests: XCTestCase {
    override func setUp() { StubProtocol.reset() }

    func testDiscoveryAndSessionsUseOnlyV2Routes() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"terminal":true,"conversation":false},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """)
        StubProtocol.stub(path: "/v2/sessions", body: #"{"sessions":[]}"#)
        let gateway = LatchGateway(
            link: try GatewayLink(address: "http://127.0.0.1:8787", token: "token"),
            session: StubProtocol.session()
        )
        _ = try await gateway.listSessions()
        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities", "/v2/sessions"])
    }

    func testUnknownMessageStatusFallsBackToComplete() throws {
        let status = try JSONDecoder().decode(MessageStatus.self, from: Data(#""future""#.utf8))
        XCTAssertEqual(status, .complete)
    }

    func testDirectoryBrowsingEncodesPathsAndPaginationWithAuthorization() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities())
        StubProtocol.stub(path: "/v2/directories", body: """
        {"path":"/tmp/日本 #1","parent":"/tmp","entries":[],"nextCursor":null}
        """)
        let gateway = makeGateway()

        let page = try await gateway.browseDirectories(path: "/tmp/日本 #1", cursor: "after+/=")

        XCTAssertEqual(page.path, "/tmp/日本 #1")
        let request = try XCTUnwrap(StubProtocol.requests.last)
        var components = URLComponents()
        components.percentEncodedQuery = request.query
        XCTAssertEqual(components.queryItems?.first(where: { $0.name == "path" })?.value, "/tmp/日本 #1")
        XCTAssertEqual(components.queryItems?.first(where: { $0.name == "cursor" })?.value, "after+/=")
        XCTAssertEqual(request.headers["Authorization"], "Bearer token")
    }

    func testCreationPostsOnlyTheTypedJSONRequest() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities())
        StubProtocol.stub(path: "/v2/sessions", body: """
        {"protocolVersion":2,"session":{"id":"ses_new","name":"shell",
        "state":"running","createdAt":"2026-09-06T00:00:00Z"}}
        """)
        let gateway = makeGateway()
        let requestID = try XCTUnwrap(UUID(uuidString: "8cba5d78-79a0-4a55-9047-f77e57e463c7"))

        let report = try await gateway.createSession(requestID: requestID, cwd: "/tmp/a b")

        XCTAssertEqual(report.session.id, "ses_new")
        let request = try XCTUnwrap(StubProtocol.requests.last)
        XCTAssertEqual(request.method, "POST")
        XCTAssertEqual(request.headers["Content-Type"], "application/json")
        let body = try JSONDecoder().decode(CreateSessionRequest.self, from: Data(request.body.utf8))
        XCTAssertEqual(body, CreateSessionRequest(requestId: requestID, cwd: "/tmp/a b"))
    }

    func testUndiscoveredDirectoryAndCreationRoutesAreNeverProbed() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities(browse: false, create: false))
        let gateway = makeGateway()

        do {
            _ = try await gateway.browseDirectories()
            XCTFail("browse should be gated by discovery")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.browseDirectories))
        }
        do {
            _ = try await gateway.createSession(requestID: UUID(), cwd: "/tmp")
            XCTFail("creation should be gated by discovery")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.createSession))
        }
        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities"])
    }

    func testStableUnreadableDirectoryErrorIsNotMistakenForBadAuthorization() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities())
        StubProtocol.stub(
            path: "/v2/directories",
            status: 403,
            body: #"{"error":"unreadable_directory","reason":"directory cannot be read"}"#
        )
        do {
            _ = try await makeGateway().browseDirectories(path: "/private")
            XCTFail("unreadable directory should fail")
        } catch let error as LatchError {
            guard case .http(let status, let path, let reason) = error else {
                return XCTFail("unexpected error: \(error)")
            }
            XCTAssertEqual(status, 403)
            XCTAssertEqual(path, "/v2/directories")
            XCTAssertEqual(reason, "directory cannot be read")
        }
    }

    private func makeGateway() -> LatchGateway {
        LatchGateway(
            link: try! GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )
    }

    private func capabilities(browse: Bool = true, create: Bool = true) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"terminal":true,"conversation":false,
          "browseDirectories":\(browse),"createSession":\(create)},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }
}
