import XCTest

@testable import LatchMobileKit

/// The QR payload is untrusted input read many times a second from whatever is
/// in front of the camera, so every rejection it makes is asserted here.
final class PairingPayloadTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    private func code(
        formatVersion: Int = 1,
        pairingId: String = "0123456789abcdef0123456789abcdef",
        secret: String = String(repeating: "a", count: 64),
        macPublicKey: String = String(repeating: "1", count: 64),
        expiresIn: TimeInterval = 240,
        controlPlane: String? = "https://control.example",
        macName: String? = "Studio Mac",
        extra: [String: Any] = [:]
    ) -> String {
        var object: [String: Any] = [
            "formatVersion": formatVersion,
            "pairingId": pairingId,
            "secret": secret,
            "macPublicKey": macPublicKey,
            "expiresAt": now.addingTimeInterval(expiresIn).timeIntervalSince1970
        ]
        if let controlPlane { object["controlPlane"] = controlPlane }
        if let macName { object["macName"] = macName }
        object.merge(extra) { _, new in new }
        let data = try! JSONSerialization.data(withJSONObject: object)
        return String(data: data, encoding: .utf8)!
    }

    func testParsesTheMaterialTheMacEmits() throws {
        let payload = try PairingPayload.parse(code(), now: now)
        XCTAssertEqual(payload.formatVersion, 1)
        XCTAssertEqual(payload.pairingId, "0123456789abcdef0123456789abcdef")
        XCTAssertEqual(payload.macPublicKey, String(repeating: "1", count: 64))
        XCTAssertEqual(payload.macName, "Studio Mac")
        XCTAssertEqual(payload.controlPlane?.absoluteString, "https://control.example")
        XCTAssertEqual(payload.remainingLifetime(now: now), 240, accuracy: 1)
    }

    /// Unknown fields are survivable; the contract is additive.
    func testIgnoresFieldsItDoesNotKnow() throws {
        let payload = try PairingPayload.parse(
            code(extra: ["relayHint": "eu-west", "future": ["nested": true]]),
            now: now
        )
        XCTAssertEqual(payload.pairingId, "0123456789abcdef0123456789abcdef")
    }

    func testRejectsAnythingThatIsNotPairingMaterial() {
        for value in ["", "not json", "[1,2,3]", #"{"hello":"world"}"#] {
            XCTAssertThrowsError(try PairingPayload.parse(value, now: now)) { error in
                XCTAssertEqual(error as? PairingPayloadError, .notPairingMaterial)
            }
        }
    }

    /// A future format may reuse these names with different meanings, so the
    /// version gate refuses rather than guessing.
    func testRejectsAnUnsupportedFormatVersion() {
        XCTAssertThrowsError(try PairingPayload.parse(code(formatVersion: 2), now: now)) { error in
            XCTAssertEqual(error as? PairingPayloadError, .unsupportedFormat(found: 2, supported: 1))
        }
    }

    func testRejectsMalformedIdentifiersAndKeys() {
        let cases: [(String, String)] = [
            ("pairingId", code(pairingId: "abcd")),
            ("pairingId", code(pairingId: String(repeating: "z", count: 32))),
            ("secret", code(secret: String(repeating: "a", count: 62))),
            ("macPublicKey", code(macPublicKey: String(repeating: "1", count: 62)))
        ]
        for (field, value) in cases {
            XCTAssertThrowsError(try PairingPayload.parse(value, now: now)) { error in
                XCTAssertEqual(error as? PairingPayloadError, .malformedField(field), field)
            }
        }
    }

    func testAcceptsUppercaseHexAndNormalizesIt() throws {
        let payload = try PairingPayload.parse(
            code(macPublicKey: String(repeating: "A", count: 64)),
            now: now
        )
        XCTAssertEqual(payload.macPublicKey, String(repeating: "a", count: 64))
    }

    func testRejectsAnExpiredCode() {
        XCTAssertThrowsError(try PairingPayload.parse(code(expiresIn: -30), now: now)) { error in
            XCTAssertEqual(error as? PairingPayloadError, .expired(by: 30))
        }
    }

    /// The Mac grants five minutes. A code claiming a week was not made by the
    /// pairing flow it says it belongs to.
    func testRejectsALifetimeLongerThanTheMacCanGrant() {
        XCTAssertThrowsError(try PairingPayload.parse(code(expiresIn: 7 * 24 * 3600), now: now)) { error in
            XCTAssertEqual(error as? PairingPayloadError, .implausibleLifetime)
        }
    }

    func testRejectsAnUnusableControlPlaneAddress() {
        for address in ["not a url", "ftp://control.example", "https://"] {
            XCTAssertThrowsError(try PairingPayload.parse(code(controlPlane: address), now: now)) { error in
                XCTAssertEqual(error as? PairingPayloadError, .malformedField("controlPlane"), address)
            }
        }
    }

    /// The address is optional: a Mac that pairs over the local network has no
    /// control plane to name, and the caller supplies the one it already has.
    func testAllowsAMissingControlPlaneAddress() throws {
        let payload = try PairingPayload.parse(code(controlPlane: nil), now: now)
        XCTAssertNil(payload.controlPlane)
    }

    /// The name is attacker-controlled text headed for the confirmation
    /// screen, so it is bounded and stripped of control characters.
    func testRejectsAHostileMacName() {
        for name in [String(repeating: "n", count: 81), "Mac\u{0}\u{1b}[31m"] {
            XCTAssertThrowsError(try PairingPayload.parse(code(macName: name), now: now)) { error in
                XCTAssertEqual(error as? PairingPayloadError, .malformedField("macName"))
            }
        }
    }

    /// The scan and the confirmation are separated by a phrase the person has
    /// to read, and five minutes can lapse in between.
    func testLifetimeIsRecheckedAfterTheScan() throws {
        let payload = try PairingPayload.parse(code(expiresIn: 60), now: now)
        XCTAssertNoThrow(try payload.checkLifetime(now: now.addingTimeInterval(30)))
        XCTAssertThrowsError(try payload.checkLifetime(now: now.addingTimeInterval(90)))
    }

    func testDescriptionNeverCarriesTheSecret() throws {
        let secret = String(repeating: "d", count: 64)
        let payload = try PairingPayload.parse(code(secret: secret), now: now)
        XCTAssertFalse(payload.redactedDescription.contains(secret))
        XCTAssertTrue(payload.redactedDescription.contains("0123"))
    }
}
