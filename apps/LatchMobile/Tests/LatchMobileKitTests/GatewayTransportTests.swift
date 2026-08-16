import XCTest

@testable import LatchMobileKit

final class GatewayTransportTests: XCTestCase {
    func testRelayIsWithheldUntilDirectFailure() async throws {
        let policy = RemoteTransportPolicy()
        do {
            try await policy.authorizeRelayRetry()
            XCTFail("relay should be withheld")
        } catch {
            XCTAssertEqual(error as? RemoteTransportPolicyError, .relayBeforeDirectFailure)
        }
        await policy.recordDirectFailure()
        try await policy.authorizeRelayRetry()
    }

    func testPathChangeRequiresCapabilityRediscoveryOnlyOnChange() async {
        let policy = RemoteTransportPolicy()
        let firstRelay = await policy.recordSelectedPath(.relay)
        let sameRelay = await policy.recordSelectedPath(.relay)
        let changedToDirect = await policy.recordSelectedPath(.direct)
        let sameDirect = await policy.recordSelectedPath(.direct)
        XCTAssertFalse(firstRelay)
        XCTAssertFalse(sameRelay)
        XCTAssertTrue(changedToDirect)
        XCTAssertFalse(sameDirect)
    }

    func testManualTransportPreservesTheEnteredLink() throws {
        let link = try GatewayLink(address: "https://gateway.example", token: "manual-token")
        let transport = HTTPSGatewayTransport(link: link)
        XCTAssertEqual(transport.gatewayLink, link)
    }

    func testTunnelRequestWaitsForACompleteHeaderThenForwardsItOnce() throws {
        var validator = TunnelRequestValidator()
        XCTAssertNil(try validator.append(Data("GET /v1/capabilities HTTP/1.1\r\nHost: loop".utf8)))
        let first = try XCTUnwrap(try validator.append(Data("back\r\n\r\nbody".utf8)))
        XCTAssertEqual(String(data: first, encoding: .utf8), "GET /v1/capabilities HTTP/1.1\r\nHost: loopback\r\n\r\nbody")
        XCTAssertEqual(
            try validator.append(Data("later request bytes".utf8)),
            Data("later request bytes".utf8),
            "a completed loopback connection does not buffer a second request"
        )
    }

    func testTunnelRejectsAuthorizationBeforeOpeningANoiseSession() throws {
        var validator = TunnelRequestValidator()
        XCTAssertThrowsError(
            try validator.append(Data("GET / HTTP/1.1\r\nAuthorization: Bearer secret\r\n\r\n".utf8))
        ) { error in
            XCTAssertEqual(error as? NoiseTunnelError, .callerSuppliedCredential)
        }
    }

    func testTunnelRejectsProxyAuthorizationCaseInsensitively() throws {
        var validator = TunnelRequestValidator()
        XCTAssertThrowsError(
            try validator.append(Data("GET / HTTP/1.1\r\npRoXy-AuThOrIzAtIoN: Basic secret\r\n\r\n".utf8))
        ) { error in
            XCTAssertEqual(error as? NoiseTunnelError, .callerSuppliedCredential)
        }
    }
}
