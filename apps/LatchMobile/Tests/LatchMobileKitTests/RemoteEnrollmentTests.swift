import XCTest
@testable import LatchMobileKit

private final class StubEnrollmentSession: RemoteEnrollmentSession, @unchecked Sendable {
    let comparison = "0123 4567 89ab cdef"
    let record: PairedDeviceRecord
    private(set) var closed = false

    init(record: PairedDeviceRecord) { self.record = record }
    func awaitApprovedRecord() async throws -> PairedDeviceRecord { record }
    func close() async { closed = true }
}

private final class StubEnrollmentProvider: RemoteEnrollmentProviding, @unchecked Sendable {
    let session: StubEnrollmentSession
    private(set) var prepared = false
    init(session: StubEnrollmentSession) { self.session = session }
    func prepare(
        payload: PairingPayload,
        deviceName: String,
        permission: DevicePermission
    ) async throws -> any RemoteEnrollmentSession {
        prepared = true
        XCTAssertEqual(permission, .control)
        XCTAssertEqual(payload.enrollmentId, "enr_\(String(repeating: "1", count: 32))")
        return session
    }
}

@MainActor
final class RemoteEnrollmentTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let hostKey = String(repeating: "2", count: 64)

    private func code(
        version: Int = 1,
        enrollmentID: String? = nil,
        admissionCode: String? = nil,
        secret: String? = nil,
        controlPlane: String = "https://control.example",
        expiresIn: TimeInterval = 240
    ) -> String {
        let id = enrollmentID ?? "enr_\(String(repeating: "1", count: 32))"
        let object: [String: Any] = [
            "version": version,
            "controlPlane": controlPlane,
            "enrollmentId": id,
            "hostPublicKey": hostKey,
            "admissionCode": admissionCode ?? "\(id).\(String(repeating: "3", count: 64))",
            "enrollmentSecret": secret ?? String(repeating: "4", count: 64),
            "expiresAt": now.addingTimeInterval(expiresIn).timeIntervalSince1970,
            "macName": "Studio Mac",
        ]
        return String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
    }

    func testParsesOnlyTheRemoteLinkEnrollmentShape() throws {
        let payload = try PairingPayload.parse(code(), now: now)
        XCTAssertEqual(payload.version, 1)
        XCTAssertEqual(payload.controlPlane.absoluteString, "https://control.example")
        XCTAssertEqual(payload.hostPublicKey, hostKey)
        XCTAssertEqual(payload.remainingLifetime(now: now), 240, accuracy: 1)
        XCTAssertFalse(payload.redactedDescription.contains(String(repeating: "4", count: 64)))

        let old = #"{"formatVersion":1,"pairingId":"0123456789abcdef0123456789abcdef"}"#
        XCTAssertThrowsError(try PairingPayload.parse(old, now: now)) {
            XCTAssertEqual($0 as? PairingPayloadError, .notPairingMaterial)
        }
    }

    func testRejectsSubstitutionExpiryAndNonTLSServiceAddresses() {
        let wrongID = "enr_\(String(repeating: "9", count: 32))"
        let cases: [(String, PairingPayloadError)] = [
            (code(admissionCode: "\(wrongID).\(String(repeating: "3", count: 64))"), .malformedField("admissionCode")),
            (code(secret: "abcd"), .malformedField("enrollmentSecret")),
            (code(controlPlane: "http://control.example"), .malformedField("controlPlane")),
            (code(expiresIn: -1), .expired(by: 1)),
            (code(expiresIn: 86_400), .implausibleLifetime),
        ]
        for (input, expected) in cases {
            XCTAssertThrowsError(try PairingPayload.parse(input, now: now)) {
                XCTAssertEqual($0 as? PairingPayloadError, expected)
            }
        }
    }

    func testNothingPersistsBeforeComparisonAndEncryptedReceipt() async throws {
        let devices = MemoryPairedDeviceStore()
        let identities = MemoryDeviceIdentityStore()
        let record = PairedDeviceRecord(
            deviceId: "dev_controller",
            name: "iPhone",
            devicePublicKey: String(repeating: "5", count: 64),
            mac: PairedMac(deviceId: "dev_host", publicKey: hostKey, name: "Studio Mac"),
            permission: .control,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!,
            accessToken: "device-token"
        )
        let session = StubEnrollmentSession(record: record)
        let provider = StubEnrollmentProvider(session: session)
        let model = PairingModel(
            identityStore: identities,
            deviceStore: devices,
            camera: StubCameraAuthorization(.authorized),
            deviceName: "iPhone",
            enrollmentProvider: provider
        )
        await model.restore()
        model.scanned(code(), now: now)
        guard case .confirming = model.state else { return XCTFail("scan was not accepted") }
        XCTAssertNil(try devices.load())

        await model.confirm(now: now)
        guard case .comparing(_, let comparison) = model.state else {
            return XCTFail("authenticated comparison was not surfaced")
        }
        XCTAssertEqual(comparison, session.comparison)
        XCTAssertTrue(provider.prepared)
        XCTAssertNil(try devices.load())

        await model.confirmComparison()
        XCTAssertEqual(try devices.load(), record)
        XCTAssertEqual(model.record, record)
    }

    func testCancellingAuthenticatedEnrollmentClosesWithoutPersistence() async throws {
        let devices = MemoryPairedDeviceStore()
        let session = StubEnrollmentSession(record: PairedDeviceRecord(
            deviceId: "dev", name: "iPhone", devicePublicKey: String(repeating: "5", count: 64),
            mac: PairedMac(deviceId: "dev_host", publicKey: hostKey), permission: .control,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        ))
        let model = PairingModel(
            identityStore: MemoryDeviceIdentityStore(),
            deviceStore: devices,
            camera: StubCameraAuthorization(.authorized),
            enrollmentProvider: StubEnrollmentProvider(session: session)
        )
        await model.restore()
        model.scanned(code(), now: now)
        await model.confirm(now: now)
        model.cancel()
        for _ in 0..<20 where !session.closed {
            await Task.yield()
        }
        XCTAssertTrue(session.closed)
        XCTAssertNil(try devices.load())
    }
}
