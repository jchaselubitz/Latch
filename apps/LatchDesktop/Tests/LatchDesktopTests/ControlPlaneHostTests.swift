import XCTest
@testable import LatchDesktop

/// A control plane that answers from memory, so host enrollment and pairing
/// registration can be driven without a deployed service.
private final class StubControlPlaneHostAPI: ControlPlaneHostAPI, @unchecked Sendable {
    struct Recorded: Sendable {
        var accounts: [String] = []
        var enrollments: [(name: String, publicKey: String)] = []
        var rotations: [(deviceID: String, publicKey: String)] = []
        var pairings: [(pairingID: String, secretDigest: String, permission: DevicePermission)] = []
    }

    private let lock = NSLock()
    private var recorded = Recorded()
    var devices: [ControlPlaneDevice] = []
    var failure: ControlPlaneHostError?

    var log: Recorded {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func createAccount(label: String) async throws -> String {
        lock.lock()
        recorded.accounts.append(label)
        lock.unlock()
        if let failure { throw failure }
        return "acct_token"
    }

    func enrollHost(
        accountToken: String,
        name: String,
        publicKey: String
    ) async throws -> (deviceID: String, deviceToken: String) {
        lock.lock()
        recorded.enrollments.append((name, publicKey))
        lock.unlock()
        if let failure { throw failure }
        return ("dev_mac", "dev_token")
    }

    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws {
        lock.lock()
        recorded.rotations.append((deviceID, publicKey))
        lock.unlock()
        if let failure { throw failure }
    }

    func openPairingRequest(
        deviceToken: String,
        pairingID: String,
        secretDigest: String,
        permission: DevicePermission
    ) async throws {
        lock.lock()
        recorded.pairings.append((pairingID, secretDigest, permission))
        lock.unlock()
        if let failure { throw failure }
    }

    func devices(deviceToken: String) async throws -> [ControlPlaneDevice] {
        if let failure { throw failure }
        return devices
    }
}

@MainActor
final class ControlPlaneHostTests: XCTestCase {
    private let macKey = String(repeating: "1", count: 64)
    private static let suite = "co.cooperativ.latch.desktop.tests"

    /// A private suite, emptied first, so a test never reads or writes the
    /// address the app itself uses.
    private func freshDefaults() -> UserDefaults {
        let defaults = UserDefaults(suiteName: Self.suite)!
        defaults.removePersistentDomain(forName: Self.suite)
        return defaults
    }

    private func material(secret: String = String(repeating: "a", count: 64)) -> PairingMaterial {
        PairingMaterial(
            formatVersion: 1,
            pairingID: "0123456789abcdef0123456789abcdef",
            secret: secret,
            macPublicKey: macKey,
            expiresAt: 1_700_000_300,
            controlPlane: nil,
            macName: nil
        )
    }

    private func makeHost(
        api: StubControlPlaneHostAPI,
        store: MemoryHostEnrollmentStore = MemoryHostEnrollmentStore()
    ) -> ControlPlaneHost {
        ControlPlaneHost(store: store, defaults: freshDefaults(), apiFactory: { _ in api })
    }

    // MARK: - Address handling

    func testAddressIsNormalizedAndOnlyHTTPIsAccepted() throws {
        XCTAssertEqual(try ControlPlaneHost.normalize("control.example").absoluteString, "https://control.example")
        XCTAssertEqual(try ControlPlaneHost.normalize(" https://control.example/ ").absoluteString, "https://control.example")
        XCTAssertEqual(try ControlPlaneHost.normalize("http://127.0.0.1:8080").absoluteString, "http://127.0.0.1:8080")

        for bad in ["", "   ", "ftp://control.example", "https://"] {
            XCTAssertThrowsError(try ControlPlaneHost.normalize(bad), "\(bad) must be refused")
        }
    }

    func testABlankAddressClearsTheSetting() throws {
        let host = makeHost(api: StubControlPlaneHostAPI())
        try host.setAddress("control.example")
        XCTAssertTrue(host.isConfigured)
        try host.setAddress("  ")
        XCTAssertFalse(host.isConfigured)
        XCTAssertNil(host.address)
    }

    // MARK: - The secret never leaves this Mac

    /// The digest is the whole reason a control-plane breach cannot answer a
    /// scan: it must match `credentials.ts`'s domain-separated SHA-256 exactly,
    /// so this pins the vector rather than re-deriving it.
    func testSecretDigestMatchesTheControlPlaneContract() {
        XCTAssertEqual(
            ControlPlaneHost.secretDigest(String(repeating: "a", count: 64)),
            "7ce44e5edf1de426e374a315547d001dea55440ab1daf61357c51c90cc90b2a1"
        )
    }

    func testOpeningAPairingRegistersTheDigestAndNeverTheSecret() async throws {
        let api = StubControlPlaneHostAPI()
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")

        let addressed = try await host.openPairing(
            material(),
            publicKey: macKey,
            macName: "Studio Mac"
        )

        let registered = try XCTUnwrap(api.log.pairings.first)
        XCTAssertEqual(registered.pairingID, "0123456789abcdef0123456789abcdef")
        XCTAssertEqual(registered.permission, .interact)
        XCTAssertNotEqual(registered.secretDigest, String(repeating: "a", count: 64))
        XCTAssertEqual(
            registered.secretDigest,
            ControlPlaneHost.secretDigest(String(repeating: "a", count: 64))
        )

        // The code the phone scans is the one that now says where to enroll.
        XCTAssertEqual(addressed.controlPlane, "https://control.example")
        XCTAssertEqual(addressed.macName, "Studio Mac")
        XCTAssertTrue(addressed.carriesAddress)
        XCTAssertEqual(addressed.secret, material().secret)
    }

    func testPairingWithoutAConfiguredControlPlaneIsRefusedRatherThanGuessed() async {
        let host = makeHost(api: StubControlPlaneHostAPI())
        do {
            _ = try await host.openPairing(material(), publicKey: macKey, macName: "Studio Mac")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ControlPlaneHostError, .notConfigured)
        }
    }

    // MARK: - Host enrollment

    func testThisMacEnrolsOnceAndReusesItsCredentials() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")

        let first = try await host.enrollment(publicKey: macKey, name: "Studio Mac")
        let second = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        XCTAssertEqual(first, second)
        XCTAssertEqual(api.log.accounts.count, 1)
        XCTAssertEqual(api.log.enrollments.count, 1)
        XCTAssertEqual(api.log.enrollments.first?.publicKey, macKey)
        XCTAssertEqual(try store.load()?.deviceID, "dev_mac")
    }

    /// A locally rotated Mac identity is carried to the control plane rather
    /// than re-enrolled, because re-enrolling would strand this Mac's pairings.
    func testARotatedMacKeyRotatesInPlace() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore()
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        let rotated = String(repeating: "2", count: 64)
        let updated = try await host.enrollment(publicKey: rotated, name: "Studio Mac")

        XCTAssertEqual(updated.publicKey, rotated)
        XCTAssertEqual(updated.deviceID, "dev_mac")
        XCTAssertEqual(api.log.rotations.map(\.publicKey), [rotated])
        XCTAssertEqual(api.log.enrollments.count, 1)
        XCTAssertEqual(try store.load()?.publicKey, rotated)
    }

    /// Credentials issued by one deployment name nothing in another, so a
    /// changed address must enroll afresh instead of reusing a stale token.
    func testCredentialsFromAnotherControlPlaneAreNotReused() async throws {
        let api = StubControlPlaneHostAPI()
        let store = MemoryHostEnrollmentStore(
            HostEnrollment(
                address: "https://old.example",
                accountToken: "stale",
                deviceID: "dev_old",
                deviceToken: "stale",
                publicKey: macKey
            )
        )
        let host = makeHost(api: api, store: store)
        try host.setAddress("https://control.example")

        let enrollment = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        XCTAssertEqual(enrollment.address, "https://control.example")
        XCTAssertEqual(enrollment.deviceToken, "dev_token")
        XCTAssertEqual(api.log.accounts.count, 1)
        XCTAssertTrue(api.log.rotations.isEmpty)
    }

    // MARK: - Reading back the phones

    func testOnlyLivePhonesAreReportedAsPaired() async throws {
        let api = StubControlPlaneHostAPI()
        api.devices = [
            ControlPlaneDevice(
                deviceID: "dev_mac",
                name: "Studio Mac",
                role: "host",
                publicKey: macKey,
                revoked: false,
                permission: nil
            ),
            ControlPlaneDevice(
                deviceID: "dev_old",
                name: "Old phone",
                role: "client",
                publicKey: String(repeating: "3", count: 64),
                revoked: true,
                permission: .observe
            ),
            ControlPlaneDevice(
                deviceID: "dev_phone",
                name: "Jake's iPhone",
                role: "client",
                publicKey: String(repeating: "4", count: 64),
                revoked: false,
                permission: .interact
            ),
        ]
        let host = makeHost(api: api)
        try host.setAddress("https://control.example")
        _ = try await host.enrollment(publicKey: macKey, name: "Studio Mac")

        let phones = try await host.pairedClients()
        XCTAssertEqual(phones.map(\.deviceID), ["dev_phone"])
    }
}
