import XCTest
@testable import LatchMobileKit

/// The Mac refuses, rather than cleans up, any proposed device name outside
/// its allowlist. These cases come from the fixture the Mac's own tests read,
/// so a phone change that would emit a name the Mac refuses fails here.
@MainActor
final class DeviceNamePolicyTests: XCTestCase {
    private struct Fixture: Decodable {
        struct Policy: Decodable {
            let allowedPunctuation: String
            let maxBytes: Int
        }
        struct Rejected: Decodable {
            let reason: String
            let name: String
        }
        let policy: Policy
        let accepted: [String]
        let rejected: [Rejected]
    }

    private static let fixtureURL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appendingPathComponent("fixtures/remote-link/v1/device-names.json")

    private func fixture() throws -> Fixture {
        try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: Self.fixtureURL))
    }

    /// The Mac's policy, restated from the fixture.
    private func hostAccepts(_ name: String, _ policy: Fixture.Policy) -> Bool {
        let allowed = CharacterSet.letters
            .union(.decimalDigits)
            .union(CharacterSet(charactersIn: policy.allowedPunctuation))
        return !name.isEmpty
            && name.utf8.count <= policy.maxBytes
            && !name.hasPrefix(" ") && !name.hasSuffix(" ")
            && name.unicodeScalars.allSatisfy { allowed.contains($0) }
    }

    func testThePhonePolicyMatchesTheSharedFixture() throws {
        let fixture = try fixture()
        XCTAssertEqual(fixture.policy.maxBytes, PairingModel.maxEnrollableNameBytes)
        for name in fixture.accepted {
            XCTAssertEqual(PairingModel.enrollableName(name), name, "accepted name changed: \(name)")
        }
        for rejected in fixture.rejected {
            XCTAssertNotEqual(
                PairingModel.enrollableName(rejected.name), rejected.name,
                "phone would send a name the Mac refuses (\(rejected.reason))"
            )
        }
    }

    func testThePhoneNeverProposesANameTheMacRefuses() throws {
        let fixture = try fixture()
        let inputs = fixture.accepted + fixture.rejected.map(\.name)
            + ["Jake\u{2019}s iPhone \u{2014} work", String(repeating: "\u{5c0f}", count: 64)]
        for input in inputs {
            let proposed = PairingModel.enrollableName(input)
            XCTAssertTrue(hostAccepts(proposed, fixture.policy), "\(input.debugDescription) -> \(proposed.debugDescription)")
        }
    }
}
