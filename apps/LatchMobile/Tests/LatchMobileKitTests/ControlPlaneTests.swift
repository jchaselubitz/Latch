import XCTest

@testable import LatchMobileKit

/// The HTTP half of pairing, over the same stub transport the gateway client
/// is tested with. What matters here is what goes on the wire and how each
/// answer is turned into a decision the screen can act on.
final class ControlPlaneTests: XCTestCase {
    private let base = URL(string: "https://control.example")!

    override func setUp() {
        super.setUp()
        StubProtocol.reset()
    }

    override func tearDown() {
        StubProtocol.reset()
        super.tearDown()
    }

    private func client() -> HTTPControlPlaneClient {
        HTTPControlPlaneClient(baseURL: base, session: StubProtocol.session())
    }

    private var enrollment: PairingEnrollment {
        PairingEnrollment(
            pairingId: "0123456789abcdef0123456789abcdef",
            secret: String(repeating: "a", count: 64),
            devicePublicKey: String(repeating: "2", count: 64),
            deviceName: "Jake's iPhone",
            phrase: "sable-apple-maple-garnet-maple-flint"
        )
    }

    func testEnrollmentSendsTheSecretAndTheIdentityAndReadsTheGrant() async throws {
        StubProtocol.stub(
            path: "/v1/pairings/0123456789abcdef0123456789abcdef/confirm",
            body: """
            {
              "device": {
                "deviceId": "d00d",
                "name": "Jake's iPhone",
                "permission": "interact",
                "revoked": false
              },
              "mac": {
                "deviceId": "mac1",
                "publicKey": "\(String(repeating: "1", count: 64))",
                "name": "Studio Mac"
              },
              "accessToken": "token-1"
            }
            """
        )

        let confirmation = try await client().enroll(
            pairingId: enrollment.pairingId,
            enrollment: enrollment
        )
        XCTAssertEqual(confirmation.device.deviceId, "d00d")
        XCTAssertEqual(confirmation.device.permission, .interact)
        XCTAssertEqual(confirmation.mac.name, "Studio Mac")
        XCTAssertEqual(confirmation.accessToken, "token-1")

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.path, "/v1/pairings/0123456789abcdef0123456789abcdef/confirm")
        XCTAssertTrue(request.body.contains(String(repeating: "a", count: 64)))
        XCTAssertTrue(request.body.contains(String(repeating: "2", count: 64)))
        // Enrollment is authenticated by the one-time secret, not by a bearer
        // token the phone does not have yet.
        XCTAssertNil(request.headers["Authorization"])
    }

    /// Additive answers are survivable, and a permission this build cannot
    /// model degrades to the least privilege rather than to the default.
    func testUnknownFieldsAreIgnoredAndAnUnknownGrantDegradesToObserve() async throws {
        StubProtocol.stub(
            path: "/v1/pairings/\(enrollment.pairingId)/confirm",
            body: """
            {
              "device": {
                "deviceId": "d00d",
                "name": "Phone",
                "permission": "manage",
                "revoked": false,
                "future": {"nested": true}
              },
              "mac": {"publicKey": "\(String(repeating: "1", count: 64))"},
              "region": "eu-west"
            }
            """
        )
        let confirmation = try await client().enroll(
            pairingId: enrollment.pairingId,
            enrollment: enrollment
        )
        XCTAssertEqual(confirmation.device.permission, .observe)
        XCTAssertNil(confirmation.mac.name)
        XCTAssertNil(confirmation.accessToken)
    }

    /// Each of these has a different thing for the person to do, so each keeps
    /// its own case instead of collapsing into one failure string.
    func testStatusCodesBecomeTheRightDecision() async throws {
        let cases: [(Int, String, ControlPlaneError)] = [
            (404, #"{"error":"not_found"}"#, .pairingUnavailable),
            (410, #"{"error":"expired"}"#, .pairingExpired),
            (409, #"{"error":"already_paired"}"#, .alreadyPaired),
            (409, #"{"error":"consumed"}"#, .pairingUnavailable)
        ]
        for (status, body, expected) in cases {
            StubProtocol.reset()
            StubProtocol.stub(
                path: "/v1/pairings/\(enrollment.pairingId)/confirm",
                status: status,
                body: body
            )
            do {
                _ = try await client().enroll(
                    pairingId: enrollment.pairingId,
                    enrollment: enrollment
                )
                XCTFail("expected \(expected) for \(status)")
            } catch let error as ControlPlaneError {
                XCTAssertEqual(error, expected, "status \(status)")
                XCTAssertFalse(error.isRetryable, "status \(status)")
            }
        }
    }

    func testARejectedSecretIsExplicitAndNotRetryable() async throws {
        StubProtocol.stub(
            path: "/v1/pairings/\(enrollment.pairingId)/confirm",
            status: 403,
            body: #"{"error":"rejected","reason":"that secret does not match"}"#
        )
        do {
            _ = try await client().enroll(
                pairingId: enrollment.pairingId,
                enrollment: enrollment
            )
            XCTFail("expected a rejection")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .rejected("that secret does not match"))
            XCTAssertFalse(error.isRetryable)
        }
    }

    func testReadingTheDeviceCarriesTheAccessToken() async throws {
        StubProtocol.stub(
            path: "/v1/devices/d00d",
            body: """
            {
              "device": {"deviceId": "d00d", "name": "Phone", "permission": "control", "revoked": true},
              "mac": {"publicKey": "\(String(repeating: "1", count: 64))"}
            }
            """
        )
        let confirmation = try await client().device(deviceId: "d00d", accessToken: "token-1")
        XCTAssertTrue(confirmation.device.revoked)
        XCTAssertEqual(confirmation.device.permission, .control)
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }

    /// The revoke answer carries nothing, and an empty body must not read as a
    /// malformed one.
    func testRevokeAcceptsAnEmptyAnswer() async throws {
        StubProtocol.stub(path: "/v1/devices/d00d/revoke", status: 204, body: "")
        try await client().revoke(deviceId: "d00d", accessToken: "token-1")
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }
}
