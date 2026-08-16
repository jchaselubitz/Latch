import XCTest
@testable import LatchDesktop

final class RemoteAccessTests: XCTestCase {
    // MARK: - The helper never exposes `latch serve` publicly

    func testHelperLaunchNeverPublishesThePlaintextGateway() throws {
        let arguments = try RemoteAccessSupervisor.arguments(
            bind: RemoteAccessSupervisor.defaultBind
        )
        XCTAssertEqual(Array(arguments.prefix(2)), ["--bind", "0.0.0.0:0"])
        XCTAssertEqual(Array(arguments.suffix(2)), ["--latch-bin", "/usr/local/bin/latch"])
        // The desktop app must never launch the plaintext gateway itself, hand
        // it a bearer token, or opt it into a non-loopback bind. The gateway is
        // started only by the helper, on an ephemeral loopback port, with a
        // credential the helper mints per launch.
        XCTAssertFalse(arguments.contains("serve"))
        XCTAssertFalse(arguments.contains("--allow-remote"))
        XCTAssertFalse(arguments.contains("--token-file"))
        XCTAssertFalse(arguments.contains { $0.contains("127.0.0.1") })
    }

    func testHelperBindUsesAnEphemeralPortAndRefusesLoopback() {
        XCTAssertEqual(RemoteAccessSupervisor.defaultBind, "0.0.0.0:0")
        XCTAssertNoThrow(try RemoteAccessSupervisor.validate(bind: "0.0.0.0:0"))
        XCTAssertNoThrow(try RemoteAccessSupervisor.validate(bind: "[::]:0"))

        for loopback in ["127.0.0.1:0", "127.5.5.5:8080", "localhost:0", "[::1]:0"] {
            XCTAssertThrowsError(
                try RemoteAccessSupervisor.validate(bind: loopback),
                "\(loopback) must be refused"
            ) { error in
                XCTAssertEqual(
                    error as? RemoteAccessSupervisorError,
                    .unsafeBind(loopback)
                )
            }
        }
    }

    func testHostComponentHandlesIPv6AndBareHosts() {
        XCTAssertEqual(RemoteAccessSupervisor.hostComponent(of: "0.0.0.0:0"), "0.0.0.0")
        XCTAssertEqual(RemoteAccessSupervisor.hostComponent(of: "[::1]:4000"), "::1")
        XCTAssertEqual(RemoteAccessSupervisor.hostComponent(of: "192.168.1.4:0"), "192.168.1.4")
        XCTAssertTrue(RemoteAccessSupervisor.isLoopback("LOCALHOST"))
        XCTAssertFalse(RemoteAccessSupervisor.isLoopback("10.0.0.1"))
    }

    // MARK: - Permission ladder

    func testPermissionLadderMatchesTheGatewayContract() {
        XCTAssertTrue(DevicePermission.control.permits(.control))
        XCTAssertTrue(DevicePermission.control.permits(.interact))
        XCTAssertTrue(DevicePermission.control.permits(.observe))

        XCTAssertFalse(DevicePermission.interact.permits(.control))
        XCTAssertTrue(DevicePermission.interact.permits(.interact))
        XCTAssertTrue(DevicePermission.interact.permits(.observe))

        XCTAssertFalse(DevicePermission.observe.permits(.control))
        XCTAssertFalse(DevicePermission.observe.permits(.interact))
        XCTAssertTrue(DevicePermission.observe.permits(.observe))
    }

    // MARK: - CLI contracts

    func testStatusDecodesTheCLIContract() throws {
        let payload = """
        {"formatVersion":1,"enabled":true,"relayEnabled":false,\
        "deviceId":"a1b2c3","publicKey":"ff00","keyGeneration":1,\
        "pairedDevices":2,"revokedDevices":1,"listenerAddress":"192.168.1.20:49221"}
        """
        let status = try JSONDecoder().decode(
            RemoteAccessStatus.self,
            from: Data(payload.utf8)
        )
        XCTAssertTrue(status.enabled)
        XCTAssertFalse(status.relayEnabled)
        XCTAssertEqual(status.deviceID, "a1b2c3")
        XCTAssertEqual(status.listenerAddress, "192.168.1.20:49221")
        XCTAssertEqual(status.pairedDevices, 2)
    }

    func testStatusToleratesAnIdentitylessDisabledMac() throws {
        let payload = """
        {"formatVersion":1,"enabled":false,"relayEnabled":true,"deviceId":null,\
        "publicKey":null,"keyGeneration":null,"pairedDevices":0,\
        "revokedDevices":0,"listenerAddress":null}
        """
        let status = try JSONDecoder().decode(
            RemoteAccessStatus.self,
            from: Data(payload.utf8)
        )
        XCTAssertNil(status.deviceID)
        XCTAssertNil(status.listenerAddress)
    }

    func testDevicesAndAuditDecodeTheCLIContract() throws {
        let devices = try JSONDecoder().decode(
            [RemoteDevice].self,
            from: Data("""
            [{"deviceId":"aa","name":"Phone","permission":"control","revoked":false},
             {"deviceId":"bb","name":"Old","permission":"observe","revoked":true}]
            """.utf8)
        )
        XCTAssertEqual(devices.first?.permission, .control)
        XCTAssertTrue(devices.last?.revoked == true)

        let events = try JSONDecoder().decode(
            [RemoteAuditEvent].self,
            from: Data("""
            [{"timestamp":1000,"event":"connection_opened","deviceId":"aa","result":"ok"},
             {"timestamp":1001,"event":"connection_rejected","deviceId":null,"result":"timeout"}]
            """.utf8)
        )
        XCTAssertEqual(events.count, 2)
        XCTAssertTrue(events[1].isSecurityRelevant)
        XCTAssertEqual(events[0].summary, "Connection Opened")
    }

    func testPairingDocumentMatchesTheCLIShapeAndCarriesNothingElse() throws {
        let source = """
        {"formatVersion":1,"pairingId":"pid","secret":"sec",\
        "macPublicKey":"pub","expiresAt":1700000000}
        """
        let material = try JSONDecoder().decode(PairingMaterial.self, from: Data(source.utf8))
        let document = try material.pairingDocument()

        // The phone parses the CLI's camelCase JSON, so the desktop must hand
        // back that same shape rather than a format of its own.
        let decoded = try JSONSerialization.jsonObject(with: Data(document.utf8))
        let object = try XCTUnwrap(decoded as? [String: Any])
        XCTAssertEqual(
            Set(object.keys),
            ["formatVersion", "pairingId", "secret", "macPublicKey", "expiresAt"]
        )
        XCTAssertEqual(object["pairingId"] as? String, "pid")
        XCTAssertEqual(object["macPublicKey"] as? String, "pub")

        // Nothing about the loopback gateway, its token, or the Mac private key
        // may ever ride along with pairing material.
        XCTAssertFalse(document.lowercased().contains("private"))
        XCTAssertFalse(document.contains("127.0.0.1"))
        XCTAssertFalse(document.lowercased().contains("bearer"))

        // Round-tripping keeps it stable for a rescan.
        let reparsed = try JSONDecoder().decode(PairingMaterial.self, from: Data(document.utf8))
        XCTAssertEqual(reparsed, material)

        // The CLI cannot know where to enroll, so a code built from its answer
        // alone says so rather than pretending otherwise.
        XCTAssertFalse(material.carriesAddress)
    }

    /// The phone reads `controlPlane` out of the scanned document; a code
    /// without it is the "does not say where to enroll" dead end.
    func testAnAddressedPairingDocumentTellsThePhoneWhereToEnroll() throws {
        let source = """
        {"formatVersion":1,"pairingId":"pid","secret":"sec",\
        "macPublicKey":"pub","expiresAt":1700000000}
        """
        let material = try JSONDecoder().decode(PairingMaterial.self, from: Data(source.utf8))
        let addressed = material.addressed(
            to: URL(string: "https://control.example")!,
            macName: "  Studio Mac  "
        )
        let document = try addressed.pairingDocument()
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: Data(document.utf8)) as? [String: Any]
        )

        XCTAssertEqual(
            Set(object.keys),
            ["formatVersion", "pairingId", "secret", "macPublicKey", "expiresAt", "controlPlane", "macName"]
        )
        XCTAssertEqual(object["controlPlane"] as? String, "https://control.example")
        XCTAssertEqual(object["macName"] as? String, "Studio Mac")
        XCTAssertTrue(addressed.carriesAddress)
        // Adding an address must not disturb the one-time material itself.
        XCTAssertEqual(object["secret"] as? String, "sec")
        XCTAssertEqual(object["pairingId"] as? String, "pid")
        XCTAssertEqual(
            try JSONDecoder().decode(PairingMaterial.self, from: Data(document.utf8)),
            addressed
        )

        // A blank name is dropped rather than encoded as an empty string: the
        // phone's parser treats an empty name as no name either way.
        let unnamed = material.addressed(to: URL(string: "https://control.example")!, macName: "  ")
        XCTAssertNil(unnamed.macName)
    }

    func testPhaseReportsRunningOnlyWhenAListenerExists() {
        XCTAssertFalse(RemoteAccessPhase.off.isRunning)
        XCTAssertFalse(RemoteAccessPhase.starting.isRunning)
        XCTAssertFalse(RemoteAccessPhase.failed("boom").isRunning)
        XCTAssertTrue(RemoteAccessPhase.online(listener: "192.168.1.20:1").isRunning)
    }
}
