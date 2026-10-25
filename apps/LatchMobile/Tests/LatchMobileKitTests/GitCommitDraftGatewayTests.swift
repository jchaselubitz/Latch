import XCTest
@testable import LatchMobileKit

final class GitCommitDraftGatewayTests: XCTestCase {
    override func setUp() { super.setUp(); StubProtocol.reset() }

    func testDraftWireRequestCancellationAndTypedFailures() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities(support: true))
        StubProtocol.stub(path: "/v2/git/commit-message", status: 429, body: #"{"requestId":null,"code":"rate_limited","message":"sanitized","retryAfterSeconds":23}"#)
        let gateway = try gateway()
        let discovery = try await gateway.discover()
        XCTAssertEqual(discovery.features.gitCommitMessage?.state, .unconfigured)
        XCTAssertTrue(discovery.endpoints.gitCommitMessage == true, "support is separate from readiness")
        let request = GitCommitMessageRequest(requestId: UUID().uuidString,
            path: "/disposable/repo", expected: GitDraftExpected(branch: nil, head: nil, index: String(repeating: "a", count: 64)))
        do {
            _ = try await gateway.gitCommitMessage(request)
            XCTFail("expected rate limit")
        } catch let error as GitCommitDraftError {
            XCTAssertEqual(error.failure.code, .rateLimited)
            XCTAssertEqual(error.failure.retryAfterSeconds, 23)
        }
        let wire = try XCTUnwrap(StubProtocol.requests.last)
        XCTAssertEqual(wire.method, "POST")
        XCTAssertEqual(wire.path, "/v2/git/commit-message")
        XCTAssertNil(wire.query)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(wire.body.utf8)) as? [String: Any])
        XCTAssertEqual(Set(body.keys), ["requestId", "path", "expected"])
        let expected = try XCTUnwrap(body["expected"] as? [String: Any])
        XCTAssertTrue(expected["head"] is NSNull)
        XCTAssertTrue(expected["branch"] is NSNull)
        StubProtocol.stub(path: "/v2/git/commit-message/cancel", status: 204, body: "")
        try await gateway.cancelGitCommitMessage(requestID: request.requestId)
        XCTAssertEqual(StubProtocol.requests.last?.path, "/v2/git/commit-message/cancel")
    }

    func testOldGatewayNeverReceivesDraftRequest() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: capabilities(support: false))
        let gateway = try gateway()
        do {
            _ = try await gateway.gitCommitMessage(GitCommitMessageRequest(requestId: UUID().uuidString,
                path: "/disposable/repo", expected: GitDraftExpected(branch: nil, head: nil, index: "index")))
            XCTFail("old gateway must refuse generation locally")
        } catch {}
        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities"])
    }

    private func gateway() throws -> LatchGateway {
        LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: "fixture"), session: StubProtocol.session())
    }

    private func capabilities(support: Bool) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,"cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"terminal":true,"conversation":false,"gitCommitMessage":\(support)},
         "features":{"exclusiveTerminal":true,"gitCommitMessage":{"state":"unconfigured"}},
         "gatewayInstanceId":"fixture","operationRetentionSeconds":600}
        """
    }
}
