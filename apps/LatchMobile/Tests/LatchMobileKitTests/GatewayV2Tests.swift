import XCTest

@testable import LatchMobileKit

final class StubProtocol: URLProtocol {
    struct Reply { var status: Int; var body: String }
    private static let lock = NSLock()
    nonisolated(unsafe) private static var replies: [String: Reply] = [:]
    nonisolated(unsafe) private static var seen: [(path: String, query: String?, headers: [String: String], body: String)] = []

    static func stub(path: String, status: Int = 200, body: String) {
        lock.withLock { replies[path] = Reply(status: status, body: body) }
    }
    static func reset() { lock.withLock { replies = [:]; seen = [] } }
    static var requests: [(path: String, query: String?, headers: [String: String], body: String)] {
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
            URLComponents(url: $0, resolvingAgainstBaseURL: false)?.query
        }
        let body = Self.body(of: request)
        Self.lock.withLock {
            Self.seen.append((path, query, request.allHTTPHeaderFields ?? [:], String(decoding: body, as: UTF8.self)))
        }
        let found = Self.lock.withLock { Self.replies[path] }
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
}
