import Foundation
import LatchMobileKit
@testable import LatchTransportNative
import XCTest

final class DirectoryGrantTests: XCTestCase {
    private let hostKey = String(repeating: "22", count: 32)

    private func record(permission: DevicePermission = .control) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone", name: "Phone", devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: "mac", publicKey: hostKey), permission: permission,
            comparison: "0000 0000 0000 0000", controlPlane: URL(string: "https://control.example")!,
            accessToken: "token"
        )
    }

    private func entry(permission: String, key: String? = nil, revision: UInt64 = 2) -> RemoteLinkDirectoryEntry {
        RemoteLinkDirectoryEntry(
            version: 1, linkId: "link", peerDeviceId: "mac", peerPublicKey: key ?? hostKey,
            permission: permission, grantRevision: revision
        )
    }

    func testChangedGrantKeepsThePinnedPairing() throws {
        let downgraded = try NativeRemoteLinkConnector.verifiedDirectoryEntry(
            [entry(permission: "observe")], for: record(), macDeviceID: "mac"
        )
        XCTAssertEqual(downgraded.1, .observe)
        XCTAssertEqual(downgraded.0.grantRevision, 2)

        let upgraded = try NativeRemoteLinkConnector.verifiedDirectoryEntry(
            [entry(permission: "interact", revision: 3)],
            for: record(permission: .observe), macDeviceID: "mac"
        )
        XCTAssertEqual(upgraded.1, .interact)
        XCTAssertEqual(upgraded.0.grantRevision, 3)
    }

    func testChangedHostIdentityOrMissingGrantStillStops() {
        for directory in [
            [entry(permission: "observe", key: String(repeating: "33", count: 32))],
            [entry(permission: "observe", revision: 0)],
            [entry(permission: "unknown")],
            [],
        ] {
            XCTAssertThrowsError(
                try NativeRemoteLinkConnector.verifiedDirectoryEntry(directory, for: record(), macDeviceID: "mac")
            ) { error in
                guard case .revoked = error as? RemoteLinkFailure else {
                    return XCTFail("expected terminal directory rejection, got \(error)")
                }
            }
        }
    }
}
