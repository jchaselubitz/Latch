import XCTest
@testable import LatchDesktop

private final class StubControlPlaneHostAPI: ControlPlaneHostAPI, @unchecked Sendable {
    struct Completion: Equatable {
        let enrollmentID: String
        let key: String
        let permission: DevicePermission
        let revision: UInt64
    }

    var accountClaims: [(String, String)] = []
    var hostEnrollments: [(String, String, String)] = []
    var rotations: [(String, String)] = []
    var completions: [Completion] = []
    var mirrored: [(String, DevicePermission)] = []
    var revoked: [String] = []
    var relayPeers: [String] = []
    var relayEnabled: [Bool] = []
    var opened = ControlPlaneEnrollment(
        version: 1,
        enrollmentId: "enr_\(String(repeating: "1", count: 32))",
        expiresAt: 1_900_000_000,
        relayUrl: "wss://relay.example/v1/connect",
        hostPublicKey: String(repeating: "a", count: 64),
        admissionCode: "public-admission",
        hostAdmission: "host-admission"
    )
    var receipt = ControlPlaneEnrollmentReceipt(
        version: 1,
        enrollmentId: "enr_\(String(repeating: "1", count: 32))",
        hostPublicKey: String(repeating: "a", count: 64),
        controllerPublicKey: String(repeating: "b", count: 64),
        permission: .control,
        grantRevision: 4,
        remoteLinkId: "link_1"
    )
    var links: [ControlPlaneRemoteLink] = []
    var admission = ControlPlaneRelayAdmission(
        version: 1,
        relayUrl: "wss://relay.example/v1/connect",
        admission: "session-admission",
        expiresAt: 1_900_000_000
    )

    func claimAccount(invitation: String, label: String) async throws -> String {
        accountClaims.append((invitation, label))
        return "account-token"
    }

    func enrollHost(accountToken: String, name: String, publicKey: String) async throws -> (deviceID: String, deviceToken: String) {
        hostEnrollments.append((accountToken, name, publicKey))
        return ("dev_mac", "device-token")
    }

    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws {
        rotations.append((deviceID, publicKey))
    }

    func openEnrollment(deviceToken: String) async throws -> ControlPlaneEnrollment { opened }

    func completeEnrollment(
        deviceToken: String,
        enrollmentID: String,
        controllerPublicKey: String,
        permission: DevicePermission,
        grantRevision: UInt64
    ) async throws -> ControlPlaneEnrollmentReceipt {
        completions.append(Completion(
            enrollmentID: enrollmentID, key: controllerPublicKey,
            permission: permission, revision: grantRevision
        ))
        return receipt
    }

    func cancelEnrollment(deviceToken: String, enrollmentID: String) async throws {}

    func setPairingPermission(deviceToken: String, clientDeviceID: String, permission: DevicePermission) async throws {
        mirrored.append((clientDeviceID, permission))
    }

    func revokePairing(deviceToken: String, clientDeviceID: String) async throws {
        revoked.append(clientDeviceID)
    }

    func remoteLinks(deviceToken: String) async throws -> [ControlPlaneRemoteLink] { links }

    func relayAdmission(deviceToken: String, peerDeviceID: String) async throws -> ControlPlaneRelayAdmission {
        relayPeers.append(peerDeviceID)
        return admission
    }

    func renewRelayLease(deviceToken: String, leaseID: String) async throws -> ControlPlaneLeaseExtension {
        ControlPlaneLeaseExtension(leaseId: leaseID, expiresAt: 1_900_000_100, claim: "signed-extension")
    }

    func setRelayEnabled(accountToken: String, enabled: Bool) async throws { relayEnabled.append(enabled) }

    var attention: [(String, String)] = []
    func notifyAttention(deviceToken: String, clientDeviceID: String, eventID: String) async throws -> Bool {
        attention.append((clientDeviceID, eventID))
        return true
    }
}

@MainActor
final class ControlPlaneHostTests: XCTestCase {
    private let macKey = String(repeating: "a", count: 64)
    private let phoneKey = String(repeating: "b", count: 64)
    private static let suite = "co.cooperativ.latch.desktop.remote-link.tests"

    private func defaults() -> UserDefaults {
        let value = UserDefaults(suiteName: Self.suite)!
        value.removePersistentDomain(forName: Self.suite)
        return value
    }

    private func host(
        api: StubControlPlaneHostAPI,
        store: MemoryHostEnrollmentStore = MemoryHostEnrollmentStore()
    ) -> ControlPlaneHost {
        let value = ControlPlaneHost(store: store, defaults: defaults(), apiFactory: { _ in api })
        try! value.setAddress("https://control.example")
        try! value.setOwnerInvitation("inv_\(String(repeating: "c", count: 32)).\(String(repeating: "d", count: 64))")
        return value
    }

    func testAddressNormalizationAndInvitationValidation() throws {
        XCTAssertEqual(try ControlPlaneHost.normalize("control.example").absoluteString, "https://control.example")
        XCTAssertEqual(try ControlPlaneHost.normalize("http://127.0.0.1:8080/").absoluteString, "http://127.0.0.1:8080")
        XCTAssertThrowsError(try ControlPlaneHost.normalize("ftp://control.example"))
        XCTAssertThrowsError(try host(api: StubControlPlaneHostAPI()).setOwnerInvitation("not-an-invitation"))
    }

    func testHostEnrollsOnceAndRotatesItsExactKeyInPlace() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let value = host(api: api, store: store)
        _ = try await value.enrollment(publicKey: macKey, name: "Jake’s 🖥️")
        _ = try await value.enrollment(publicKey: macKey, name: "ignored")
        XCTAssertEqual(api.accountClaims.count, 1)
        XCTAssertEqual(api.hostEnrollments.count, 1)
        XCTAssertEqual(api.hostEnrollments[0].1, "Jake's")

        let rotated = String(repeating: "e", count: 64)
        _ = try await value.enrollment(publicKey: rotated, name: "ignored")
        XCTAssertEqual(api.rotations.count, 1)
        XCTAssertEqual(api.rotations[0].1, rotated)
        XCTAssertEqual(try store.load()?.publicKey, rotated)
    }

    func testEnrollmentGeneratesASeparateQRSecretAndKeepsHostAdmissionOutOfQR() async throws {
        let api = StubControlPlaneHostAPI()
        let value = host(api: api)
        let material = try await value.openRemoteEnrollment(publicKey: macKey, macName: "Studio Mac")
        XCTAssertEqual(material.enrollmentSecret.count, 64)
        XCTAssertNotEqual(material.enrollmentSecret, material.admissionCode)
        XCTAssertFalse(try material.pairingDocument().contains(material.hostAdmission))
    }

    func testEnrollmentRefusesAChangedHostKeyOrInsecureRelay() async {
        let api = StubControlPlaneHostAPI()
        api.opened = ControlPlaneEnrollment(
            version: 1, enrollmentId: "enr_bad", expiresAt: 1,
            relayUrl: "ws://relay.example", hostPublicKey: String(repeating: "f", count: 64),
            admissionCode: "public", hostAdmission: "host"
        )
        do {
            _ = try await host(api: api).openRemoteEnrollment(publicKey: macKey, macName: "Mac")
            XCTFail("substituted key and insecure relay must be refused")
        } catch {
            XCTAssertTrue(error is ControlPlaneHostError)
        }
    }

    func testCompletionAcceptsOnlyTheExactApprovedReceipt() async throws {
        let api = StubControlPlaneHostAPI()
        let value = host(api: api)
        _ = try await value.enrollment(publicKey: macKey, name: "Mac")
        _ = try await value.completeRemoteEnrollment(
            enrollmentID: api.receipt.enrollmentId,
            controllerPublicKey: phoneKey,
            permission: .control,
            grantRevision: 4
        )
        XCTAssertEqual(api.completions.first?.key, phoneKey)

        api.receipt = ControlPlaneEnrollmentReceipt(
            version: 1, enrollmentId: api.receipt.enrollmentId,
            hostPublicKey: macKey, controllerPublicKey: String(repeating: "0", count: 64),
            permission: .control, grantRevision: 4, remoteLinkId: "link_1"
        )
        do {
            _ = try await value.completeRemoteEnrollment(
                enrollmentID: api.receipt.enrollmentId,
                controllerPublicKey: phoneKey,
                permission: .control,
                grantRevision: 4
            )
            XCTFail("a substituted receipt must be refused")
        } catch {
            XCTAssertTrue(error is ControlPlaneHostError)
        }
    }

    func testCurrentLinksReceiveFreshWSSAdmissions() async throws {
        let api = StubControlPlaneHostAPI()
        api.links = [ControlPlaneRemoteLink(
            version: 1, linkId: "link_1", peerDeviceId: "dev_phone",
            peerPublicKey: phoneKey, permission: .control, grantRevision: 8
        )]
        let configurations = try await host(api: api).remoteLinkConfigurations(
            publicKey: macKey, macName: "Mac"
        )
        XCTAssertEqual(configurations.count, 1)
        XCTAssertEqual(configurations[0].peerPublicKey, phoneKey)
        XCTAssertEqual(configurations[0].grantRevision, 8)
        XCTAssertEqual(api.relayPeers, ["dev_phone"])
    }

    func testGrantMirrorAndRevocationUseTheEnrolledDeviceCredential() async throws {
        let api = StubControlPlaneHostAPI()
        let value = host(api: api)
        _ = try await value.enrollment(publicKey: macKey, name: "Mac")
        try await value.mirrorPermission(clientDeviceID: "dev_phone", permission: .interact)
        try await value.revokePairing(clientDeviceID: "dev_phone")
        XCTAssertEqual(api.mirrored.first?.0, "dev_phone")
        XCTAssertEqual(api.mirrored.first?.1, .interact)
        XCTAssertEqual(api.revoked, ["dev_phone"])
    }
}
