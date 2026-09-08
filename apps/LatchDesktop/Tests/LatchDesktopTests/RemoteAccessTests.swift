import XCTest
@testable import LatchDesktop

final class RemoteAccessTests: XCTestCase {
    private var configuration: RemoteLinkHostConfiguration {
        RemoteLinkHostConfiguration(
            relayUrl: "wss://relay.example/v1/connect",
            admission: "signed-secret-admission",
            peerPublicKey: String(repeating: "a", count: 64),
            grantRevision: 7
        )
    }

    func testHelperLaunchNeverPublishesThePlaintextGatewayOrAdmission() throws {
        let arguments = try RemoteAccessSupervisor.arguments()
        XCTAssertEqual(arguments, ["--link-serve", "--latch-bin", "/usr/local/bin/latch"])
        XCTAssertFalse(arguments.contains("serve"))
        XCTAssertFalse(arguments.contains("--allow-remote"))
        XCTAssertFalse(arguments.contains("--token-file"))
        XCTAssertFalse(arguments.contains("signed-secret-admission"))

        let encoded = try JSONEncoder().encode(configuration)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        XCTAssertEqual(object["admission"] as? String, "signed-secret-admission")
        XCTAssertEqual(object["grantRevision"] as? Int, 7)
    }

    func testStatusDecodesOnlyLocalAuthorityState() throws {
        let payload = """
        {"formatVersion":1,"enabled":true,"deviceId":"dev_mac",\
        "publicKey":"ff00","keyGeneration":2,"pairedDevices":2,"revokedDevices":1}
        """
        let status = try JSONDecoder().decode(RemoteAccessStatus.self, from: Data(payload.utf8))
        XCTAssertTrue(status.enabled)
        XCTAssertEqual(status.deviceID, "dev_mac")
        XCTAssertEqual(status.keyGeneration, 2)
        XCTAssertEqual(status.pairedDevices, 2)
    }

    func testDeviceCarriesTheCurrentGrantRevisionAndDirectoryIdentity() throws {
        let payload = """
        {"deviceId":"dev_local","name":"Phone","permission":"control",\
        "revoked":false,"grantRevision":9,"controlPlaneDeviceId":"dev_cloud"}
        """
        let device = try JSONDecoder().decode(RemoteDevice.self, from: Data(payload.utf8))
        XCTAssertEqual(device.grantRevision, 9)
        XCTAssertEqual(device.controlPlaneDeviceID, "dev_cloud")
        XCTAssertTrue(device.allowsTerminal)
    }

    func testPermissionLadderMatchesTheGatewayContract() {
        XCTAssertTrue(DevicePermission.control.permits(.observe))
        XCTAssertTrue(DevicePermission.control.permits(.interact))
        XCTAssertTrue(DevicePermission.control.permits(.control))
        XCTAssertTrue(DevicePermission.interact.permits(.observe))
        XCTAssertFalse(DevicePermission.interact.permits(.control))
        XCTAssertFalse(DevicePermission.observe.permits(.interact))
    }

    func testEnrollmentQRContainsTheSeparateSecretButNotTheHostAdmission() throws {
        let material = RemoteEnrollmentMaterial(
            enrollmentID: "enr_\(String(repeating: "1", count: 32))",
            enrollmentSecret: String(repeating: "2", count: 64),
            hostPublicKey: String(repeating: "3", count: 64),
            admissionCode: "public-admission",
            hostAdmission: "host-only-admission",
            relayURL: "wss://relay.example/v1/connect",
            expiresAt: 1_900_000_000,
            controlPlane: "https://control.example",
            macName: "Studio Mac"
        )
        let document = try material.pairingDocument()
        XCTAssertTrue(document.contains(String(repeating: "2", count: 64)))
        XCTAssertTrue(document.contains("public-admission"))
        XCTAssertFalse(document.contains("host-only-admission"))
        XCTAssertFalse(document.contains(material.relayURL))
    }

    func testPendingEnrollmentCarriesExactKeyGrantAndComparison() throws {
        let key = String(repeating: "a", count: 64)
        let payload = """
        {"type":"enrollment_pending","version":1,"enrollmentId":"enr_1",\
        "provisionalDeviceId":"dev_phone","controllerPublicKey":"\(key)",\
        "name":"Phone","permission":"control","comparison":"0123 4567 89ab cdef"}
        """
        let pending = try JSONDecoder().decode(RemoteEnrollmentPending.self, from: Data(payload.utf8))
        XCTAssertEqual(pending.controllerPublicKey, key)
        XCTAssertEqual(pending.permission, .control)
        XCTAssertEqual(pending.comparison, "0123 4567 89ab cdef")
    }
}

final class RemoteLinkLifecycleTests: XCTestCase {
    func testHelperEventsAreClassifiedWithoutTrustingUnknownStatuses() {
        XCTAssertEqual(
            RemoteAccessSupervisor.parseEvent(#"{"type":"status","version":1,"status":"ready","carrier":"relay"}"#),
            .status(.ready)
        )
        XCTAssertEqual(
            RemoteAccessSupervisor.parseEvent(#"{"type":"status","version":1,"status":"link_closed","reason":"peer_gone"}"#),
            .status(.linkClosed)
        )
        XCTAssertEqual(
            RemoteAccessSupervisor.parseEvent(#"{"type":"lease_started","version":1,"leaseId":"lease_abc","expiresAt":1900000000}"#),
            .leaseStarted(leaseID: "lease_abc", expiresAt: 1_900_000_000)
        )
        XCTAssertEqual(
            RemoteAccessSupervisor.parseEvent(#"{"type":"admission_needed","version":1,"reason":"link_closed"}"#),
            .admissionNeeded(reason: "link_closed")
        )
        XCTAssertEqual(
            RemoteAccessSupervisor.parseEvent(#"{"type":"status","version":1,"status":"teleporting"}"#),
            .ignored
        )
        XCTAssertNil(RemoteAccessSupervisor.parseEvent(#"{"type":"status","version":2,"status":"ready"}"#))
        XCTAssertNil(RemoteAccessSupervisor.parseEvent("not json"))
    }

    func testHelperCommandsAreVersionedLinesWithoutExtraFields() throws {
        let admission = HelperCommand.admission(relayURL: "wss://relay.example/v1/connect", admission: "ticket")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: admission.dropLast()) as? [String: Any])
        XCTAssertEqual(object["type"] as? String, "admission")
        XCTAssertEqual(object["version"] as? Int, 1)
        XCTAssertEqual(object["admission"] as? String, "ticket")
        XCTAssertEqual(admission.last, 0x0a)
        let extensionLine = HelperCommand.leaseExtension(leaseID: "lease_abc", claim: "signed")
        let extended = try XCTUnwrap(JSONSerialization.jsonObject(with: extensionLine.dropLast()) as? [String: Any])
        XCTAssertEqual(extended.keys.sorted(), ["claim", "leaseId", "type", "version"])
    }

    func testKeepAwakeRequiresOptInExternalPowerAndAConnectedPhone() {
        XCTAssertFalse(SleepPolicy.shouldPreventSleep(keepAwake: false, externalPower: true, connectedPeers: 1))
        XCTAssertFalse(SleepPolicy.shouldPreventSleep(keepAwake: true, externalPower: false, connectedPeers: 1))
        // A waiting relay socket is not a connection.
        XCTAssertFalse(SleepPolicy.shouldPreventSleep(keepAwake: true, externalPower: true, connectedPeers: 0))
        XCTAssertTrue(SleepPolicy.shouldPreventSleep(keepAwake: true, externalPower: true, connectedPeers: 1))
    }

    func testAttentionForwarderMapsAcknowledgesAndRetainsOnTransientFailure() async {
        final class Recorder: @unchecked Sendable {
            var acknowledged: [String] = []
            var notified: [(String, String)] = []
            var fail = false
        }
        let recorder = Recorder()
        let events = [
            RemoteAttentionEvent(eventID: "1".repeat32, deviceID: "local-a", kind: "completion", createdAt: 1),
            RemoteAttentionEvent(eventID: "2".repeat32, deviceID: "unknown", kind: "approval", createdAt: 2),
        ]
        let forwarder = AttentionForwarder(
            fetch: { events },
            acknowledge: { recorder.acknowledged.append($0) },
            directoryID: { $0 == "local-a" ? "dev_cloud" : nil },
            notify: { client, event in
                if recorder.fail { throw ControlPlaneHostError.transport("down") }
                recorder.notified.append((client, event))
            }
        )
        var outcome = await forwarder.forwardOnce()
        XCTAssertEqual(outcome, AttentionForwarder.Outcome(forwarded: 1, skipped: 1, retained: 0))
        XCTAssertEqual(recorder.notified.map(\.0), ["dev_cloud"])
        XCTAssertEqual(recorder.acknowledged.sorted(), ["1".repeat32, "2".repeat32].sorted())

        recorder.fail = true
        recorder.acknowledged = []
        outcome = await forwarder.forwardOnce()
        // A transient failure keeps the entry for the next poll; the unknown
        // device is still acknowledged because nobody will ever receive it.
        XCTAssertEqual(outcome, AttentionForwarder.Outcome(forwarded: 0, skipped: 1, retained: 1))
        XCTAssertEqual(recorder.acknowledged, ["2".repeat32])
    }
}

private extension String {
    var repeat32: String { String(repeating: self, count: 32) }
}
