import XCTest

@testable import LatchMobileKit

/// A control plane that answers from a script, so the whole pairing flow can
/// be driven without a service.
private final class StubControlPlane: ControlPlaneClient, @unchecked Sendable {
    struct Recorded: Sendable {
        var enrollments: [(pairingId: String, enrollment: PairingEnrollment)] = []
        var revocations: [String] = []
        var reads: [String] = []
    }

    private let lock = NSLock()
    private var recorded = Recorded()
    var confirmation: PairingConfirmation?
    var refreshed: PairingConfirmation?
    var failure: ControlPlaneError?
    var refreshFailure: ControlPlaneError?

    var log: Recorded {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func enroll(pairingId: String, enrollment: PairingEnrollment) async throws -> PairingConfirmation {
        lock.withLock {
            recorded.enrollments.append((pairingId, enrollment))
        }
        if let failure { throw failure }
        guard let confirmation else { throw ControlPlaneError.pairingUnavailable }
        return confirmation
    }

    func device(deviceId: String, accessToken: String) async throws -> PairingConfirmation {
        lock.withLock {
            recorded.reads.append(deviceId)
        }
        if let refreshFailure { throw refreshFailure }
        guard let answer = refreshed ?? confirmation else {
            throw ControlPlaneError.pairingUnavailable
        }
        return answer
    }

    func revoke(deviceId: String, accessToken: String) async throws {
        lock.withLock {
            recorded.revocations.append(deviceId)
        }
        if let failure { throw failure }
    }
}

/// Records which address the flow actually built a client for, which is the
/// only way to tell a code's address from a typed one apart.
private final class ControlPlaneAddressRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var addresses: [URL] = []

    func record(_ address: URL) {
        lock.lock()
        defer { lock.unlock() }
        addresses.append(address)
    }

    var used: [URL] {
        lock.lock()
        defer { lock.unlock() }
        return addresses
    }
}

@MainActor
final class PairingFlowTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let macKey = String(repeating: "1", count: 64)

    private func code(
        macPublicKey: String? = nil,
        expiresIn: TimeInterval = 240,
        controlPlane: String? = "https://control.example",
        formatVersion: Int = 1
    ) -> String {
        var object: [String: Any] = [
            "formatVersion": formatVersion,
            "pairingId": "0123456789abcdef0123456789abcdef",
            "secret": String(repeating: "a", count: 64),
            "macPublicKey": macPublicKey ?? macKey,
            "expiresAt": now.addingTimeInterval(expiresIn).timeIntervalSince1970,
            "macName": "Studio Mac"
        ]
        if let controlPlane { object["controlPlane"] = controlPlane }
        return String(data: try! JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }

    private func confirmation(
        permission: DevicePermission = .interact,
        revoked: Bool = false,
        macPublicKey: String? = nil
    ) -> PairingConfirmation {
        PairingConfirmation(
            device: .init(
                deviceId: "d00d",
                name: "Jake's iPhone",
                permission: permission,
                revoked: revoked
            ),
            mac: .init(deviceId: "mac1", publicKey: macPublicKey ?? macKey, name: "Studio Mac"),
            accessToken: "token-1"
        )
    }

    private func makeModel(
        control: StubControlPlane,
        devices: MemoryPairedDeviceStore = MemoryPairedDeviceStore(),
        identities: MemoryDeviceIdentityStore = MemoryDeviceIdentityStore(),
        camera: CameraPermission = .authorized,
        addresses: MemoryControlPlaneAddressStore = MemoryControlPlaneAddressStore(),
        addressesUsed: ControlPlaneAddressRecorder? = nil
    ) -> PairingModel {
        PairingModel(
            identityStore: identities,
            deviceStore: devices,
            camera: StubCameraAuthorization(camera),
            deviceName: "Jake's iPhone",
            addressStore: addresses,
            clientFactory: { address in
                addressesUsed?.record(address)
                return control
            }
        )
    }

    // MARK: - The whole flow

    func testScanConfirmAndPersist() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let devices = MemoryPairedDeviceStore()
        let model = makeModel(control: control, devices: devices)
        await model.restore()

        model.scanned(code(), now: now)
        guard case .confirming(let proposal) = model.state else {
            return XCTFail("expected a confirmation, got \(model.state)")
        }
        XCTAssertEqual(proposal.macDisplayName, "Studio Mac")
        XCTAssertEqual(
            proposal.phrase,
            PairingPhrase.derive(
                pairingId: "0123456789abcdef0123456789abcdef",
                macPublicKey: macKey,
                devicePublicKey: model.identity!.publicKey
            )
        )

        await model.confirm(now: now)
        guard case .paired(let record) = model.state else {
            return XCTFail("expected a pairing, got \(model.state)")
        }
        XCTAssertEqual(record.deviceId, "d00d")
        XCTAssertEqual(record.mac.publicKey, macKey)
        XCTAssertEqual(record.permission, .interact)
        XCTAssertEqual(record.phrase, proposal.phrase)
        XCTAssertEqual(record.accessToken, "token-1")

        // The record has to survive relaunch: this is the whole point of
        // persisting it rather than holding it in the model.
        XCTAssertEqual(try devices.load(), record)
        let relaunched = makeModel(control: control, devices: devices)
        await relaunched.restore()
        XCTAssertEqual(relaunched.record, record)

        // The enrollment carried the secret and this phone's identity, and
        // nothing else was invented for it.
        let enrollment = control.log.enrollments.first?.enrollment
        XCTAssertEqual(enrollment?.secret, String(repeating: "a", count: 64))
        XCTAssertEqual(enrollment?.devicePublicKey, model.identity?.publicKey)
        XCTAssertEqual(enrollment?.deviceName, "Jake's iPhone")
        XCTAssertEqual(enrollment?.phrase, proposal.phrase)
    }

    /// The pinned key is what pairing exists to move. A control plane that
    /// answers with a different Mac is refused, and nothing is saved.
    func testAControlPlaneThatSubstitutesTheMacIdentityIsRefused() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation(macPublicKey: String(repeating: "2", count: 64))
        let devices = MemoryPairedDeviceStore()
        let model = makeModel(control: control, devices: devices)
        await model.restore()

        model.scanned(code(), now: now)
        await model.confirm(now: now)

        guard case .failed(let message) = model.state else {
            return XCTFail("expected a refusal, got \(model.state)")
        }
        XCTAssertTrue(message.contains("do not confirm"), message)
        XCTAssertNil(try devices.load())
        XCTAssertNil(model.record)
    }

    /// Reading the phrase takes time, and five minutes is short enough to
    /// lapse while it is being read.
    func testExpiryIsRecheckedBeforeEnrolling() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()

        model.scanned(code(expiresIn: 60), now: now)
        await model.confirm(now: now.addingTimeInterval(120))

        guard case .failed(let message) = model.state else {
            return XCTFail("expected an expiry, got \(model.state)")
        }
        XCTAssertTrue(message.contains("expired"), message)
        XCTAssertTrue(control.log.enrollments.isEmpty)
    }

    func testEnrollmentNeedsAnAddressToEnrollAgainst() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()

        model.scanned(code(controlPlane: nil), now: now)
        await model.confirm(now: now)

        guard case .failed(let message) = model.state else {
            return XCTFail("expected a refusal, got \(model.state)")
        }
        XCTAssertEqual(message, ControlPlaneError.noAddress.message)
    }

    /// A Mac with no control plane configured still produces a usable code:
    /// everything but the address is in it, and the address can be supplied on
    /// the phone. This is the recovery path for the reported failure.
    func testATypedAddressCompletesACodeThatCarriesNone() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let addresses = MemoryControlPlaneAddressStore()
        let used = ControlPlaneAddressRecorder()
        let model = makeModel(control: control, addresses: addresses, addressesUsed: used)
        await model.restore()

        model.scanned(code(controlPlane: nil), now: now)
        XCTAssertTrue(model.needsControlPlaneAddress)

        // Typed without a scheme, the way it is read off a screen.
        model.manualControlPlane = "control.example"
        XCTAssertFalse(model.needsControlPlaneAddress)

        await model.confirm(now: now)
        guard case .paired = model.state else {
            return XCTFail("expected a pairing, got \(model.state)")
        }
        XCTAssertEqual(used.used.map(\.absoluteString), ["https://control.example"])
        // Remembered, so the next code from the same Mac does not ask again.
        XCTAssertEqual(addresses.load()?.absoluteString, "https://control.example")
    }

    /// A typed address must never redirect a code that names its own: the
    /// code came from the Mac being paired with, and the typed value did not.
    func testACodeWithAnAddressIgnoresTheTypedOne() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let addresses = MemoryControlPlaneAddressStore()
        let used = ControlPlaneAddressRecorder()
        let model = makeModel(control: control, addresses: addresses, addressesUsed: used)
        await model.restore()
        model.manualControlPlane = "https://typed.example"

        model.scanned(code(controlPlane: "https://control.example"), now: now)
        XCTAssertFalse(model.needsControlPlaneAddress)
        await model.confirm(now: now)

        guard case .paired = model.state else {
            return XCTFail("expected a pairing, got \(model.state)")
        }
        XCTAssertEqual(used.used.map(\.absoluteString), ["https://control.example"])
        // Nothing was learned from a code that already knew where to go.
        XCTAssertNil(addresses.load())
    }

    /// The control plane answers a name outside its label set with a 400, and
    /// "the control plane refused this" is the wrong thing to tell someone
    /// whose phone is called "Jake’s iPhone".
    func testTheEnrolledNameIsReducedToWhatTheServiceAccepts() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()
        model.deviceName = "Jake’s iPhone 📱"

        model.scanned(code(), now: now)
        await model.confirm(now: now)

        let enrolled = try XCTUnwrap(control.log.enrollments.first)
        // The apostrophe iOS puts in the name is folded to the one the service
        // accepts rather than dropped, so the phone keeps its name.
        XCTAssertEqual(enrolled.enrollment.deviceName, "Jake's iPhone")
    }

    func testAnUnusableNameFallsBackRatherThanEnrollingBlank() {
        XCTAssertEqual(PairingModel.enrollableName("🙂🙂"), PairingModel.defaultDeviceName)
        XCTAssertEqual(PairingModel.enrollableName("   "), PairingModel.defaultDeviceName)
        XCTAssertEqual(PairingModel.enrollableName("Jake's iPhone"), "Jake's iPhone")
        XCTAssertEqual(PairingModel.enrollableName("Jake\u{2019}s iPhone"), "Jake's iPhone")
        XCTAssertEqual(PairingModel.enrollableName("iPhone \u{2014} 15 Pro"), "iPhone - 15 Pro")
        XCTAssertEqual(PairingModel.enrollableName("Ana\u{0301}s iPhone"), "An\u{00E1}s iPhone")
        XCTAssertEqual(PairingModel.enrollableName(String(repeating: "a", count: 100)).count, 64)
    }

    func testATypedAddressIsRefusedUnlessItIsUsable() {
        XCTAssertNil(ControlPlaneAddress.parse(""))
        XCTAssertNil(ControlPlaneAddress.parse("   "))
        XCTAssertNil(ControlPlaneAddress.parse("ftp://control.example"))
        XCTAssertNil(ControlPlaneAddress.parse("https://"))
        XCTAssertEqual(
            ControlPlaneAddress.parse(" control.example/ ")?.absoluteString,
            "https://control.example"
        )
        XCTAssertEqual(
            ControlPlaneAddress.parse("http://127.0.0.1:8080")?.absoluteString,
            "http://127.0.0.1:8080"
        )
    }

    func testAConsumedPairingReportsWhatToDoAboutIt() async throws {
        let control = StubControlPlane()
        control.failure = .pairingUnavailable
        let model = makeModel(control: control)
        await model.restore()

        model.scanned(code(), now: now)
        await model.confirm(now: now)

        guard case .failed(let message) = model.state else {
            return XCTFail("expected a refusal, got \(model.state)")
        }
        XCTAssertTrue(message.contains("Show a new one"), message)
    }

    // MARK: - Scanning behavior

    /// The scanner re-reads the same code many times a second. Without the
    /// guard, a bad code would report its failure at that rate.
    func testTheSameCodeIsOnlyHandledOnce() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()
        await model.beginScanning()

        let scanned = code()
        model.scanned(scanned, now: now)
        guard case .confirming = model.state else {
            return XCTFail("expected a confirmation, got \(model.state)")
        }
        // A repeat while confirming must not reset the screen the person is
        // in the middle of reading.
        model.scanned(scanned, now: now)
        guard case .confirming = model.state else {
            return XCTFail("the repeat restarted the flow")
        }
    }

    func testAForeignOrOutdatedCodeSaysWhichItIs() async throws {
        let control = StubControlPlane()
        let model = makeModel(control: control)
        await model.restore()
        await model.beginScanning()

        model.scanned("https://example.com", now: now)
        XCTAssertEqual(model.state, .failed(PairingPayloadError.notPairingMaterial.message))

        model.scanned(code(formatVersion: 9), now: now)
        XCTAssertEqual(
            model.state,
            .failed(PairingPayloadError.unsupportedFormat(found: 9, supported: 1).message)
        )
    }

    // MARK: - Permission state

    func testCameraPermissionIsReportedRatherThanFailingSilently() async throws {
        let control = StubControlPlane()
        let denied = makeModel(control: control, camera: .denied)
        await denied.restore()
        await denied.beginScanning()
        XCTAssertEqual(denied.cameraPermission, .denied)
        XCTAssertFalse(denied.cameraPermission.allowsScanning)
        XCTAssertNotNil(denied.cameraPermission.explanation)
        // The screen still opens: the code can be typed in by hand.
        XCTAssertEqual(denied.state, .scanning)

        let allowed = makeModel(control: control, camera: .authorized)
        await allowed.restore()
        await allowed.beginScanning()
        XCTAssertTrue(allowed.cameraPermission.allowsScanning)
        XCTAssertNil(allowed.cameraPermission.explanation)
    }

    func testPermissionChangesOnTheMacAreAdoptedOnRefresh() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation(permission: .observe)
        let devices = MemoryPairedDeviceStore()
        let model = makeModel(control: control, devices: devices)
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)
        XCTAssertEqual(model.record?.permission, .observe)

        control.refreshed = confirmation(permission: .control)
        await model.refreshPermission()
        XCTAssertEqual(model.record?.permission, .control)
        XCTAssertEqual(try devices.load()?.permission, .control)
        XCTAssertTrue(model.record!.permission.permits(.interact))
    }

    func testARevokeOnTheMacIsSurfacedAndPersisted() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let devices = MemoryPairedDeviceStore()
        let model = makeModel(control: control, devices: devices)
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)

        control.refreshed = confirmation(revoked: true)
        await model.refreshPermission()

        guard case .revoked(let record) = model.state else {
            return XCTFail("expected a revocation, got \(model.state)")
        }
        XCTAssertTrue(record.revoked)
        XCTAssertFalse(record.isActive)
        // The now-useless credential is not kept on disk.
        XCTAssertNil(record.accessToken)
        XCTAssertEqual(try devices.load()?.revoked, true)

        // And it survives relaunch as a revocation rather than as a pairing.
        let relaunched = makeModel(control: control, devices: devices)
        await relaunched.restore()
        guard case .revoked = relaunched.state else {
            return XCTFail("expected a revocation after relaunch, got \(relaunched.state)")
        }
    }

    /// A control plane that no longer recognizes the device is a revocation
    /// from the phone's point of view.
    func testAnUnrecognizedDeviceIsTreatedAsRevoked() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)

        control.refreshFailure = .rejected("no such device")
        await model.refreshPermission()
        guard case .revoked = model.state else {
            return XCTFail("expected a revocation, got \(model.state)")
        }
    }

    /// A refresh is advisory. A flaky network must not make a working pairing
    /// look revoked.
    func testANetworkFailureDoesNotLookLikeARevocation() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let model = makeModel(control: control)
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)

        control.refreshFailure = .transport("the network is offline")
        await model.refreshPermission()
        guard case .paired = model.state else {
            return XCTFail("expected the pairing to hold, got \(model.state)")
        }
    }

    // MARK: - Revoking from the phone

    func testRevokingFromThePhoneClearsTheRecordAndTheIdentity() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let devices = MemoryPairedDeviceStore()
        let identities = MemoryDeviceIdentityStore()
        let model = makeModel(control: control, devices: devices, identities: identities)
        await model.restore()
        let original = model.identity?.publicKey
        model.scanned(code(), now: now)
        await model.confirm(now: now)

        await model.revoke()

        XCTAssertEqual(control.log.revocations, ["d00d"])
        XCTAssertNil(try devices.load())
        XCTAssertNil(model.record)
        XCTAssertEqual(model.state, .idle)
        // The identity the Mac pinned is gone, so pairing again needs a new
        // code and enrolls a new key.
        XCTAssertNotEqual(model.identity?.publicKey, original)
    }

    /// The local half of a revoke must not depend on reaching the service.
    func testRevokingWorksWhenTheControlPlaneIsUnreachable() async throws {
        let control = StubControlPlane()
        control.confirmation = confirmation()
        let devices = MemoryPairedDeviceStore()
        let model = makeModel(control: control, devices: devices)
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)

        control.failure = .transport("the network is offline")
        await model.revoke()

        XCTAssertNil(try devices.load())
        XCTAssertNil(model.record)
        XCTAssertEqual(model.state, .idle)
    }
}
