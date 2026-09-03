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

    func testHelperReceivesStunServersAndNeverARelay() throws {
        let arguments = try RemoteAccessSupervisor.arguments(
            bind: RemoteAccessSupervisor.defaultBind,
            iceServers: ["stun:stun.cloudflare.com:3478", "stuns:stun.example:5349"]
        )
        XCTAssertEqual(
            arguments,
            [
                "--bind", "0.0.0.0:0", "--latch-bin", "/usr/local/bin/latch",
                "--ice-server", "stun:stun.cloudflare.com:3478",
                "--ice-server", "stuns:stun.example:5349",
            ]
        )
        // A relay allocation is the phone's decision under the phone's
        // credentials. The helper refuses a TURN URL; the app refuses it
        // first so that contract cannot be loosened by a control-plane reply.
        XCTAssertThrowsError(
            try RemoteAccessSupervisor.arguments(
                bind: RemoteAccessSupervisor.defaultBind,
                iceServers: ["turn:turn.cloudflare.com:3478?transport=udp"]
            )
        ) { error in
            XCTAssertEqual(
                error as? RemoteAccessSupervisorError,
                .relayServerRefused("turn:turn.cloudflare.com:3478?transport=udp")
            )
        }
        XCTAssertTrue(ControlPlaneIceServer.isStun("STUN:stun.example:3478"))
        XCTAssertFalse(ControlPlaneIceServer.isStun("turns:relay.example:443"))
        XCTAssertFalse(ControlPlaneIceServer.isStun("stun.example:3478"))
    }

    func testMissingHelperIsNamedInsteadOfARawLaunchError() {
        let url = URL(fileURLWithPath: "/tmp/missing-latch-remote")
        XCTAssertEqual(
            RemoteAccessSupervisorError.helperMissing(url).localizedDescription,
            "The remote-access helper is missing or not executable at \(url.path). It is installed next to the Latch CLI; run `latch update` or the installer in Settings → Latch CLI to repair the complete payload."
        )
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

    func testStatusCarriesTheHelpersGatheredIceAgent() throws {
        let payload = """
        {"formatVersion":1,"enabled":true,"relayEnabled":false,\
        "deviceId":"a1b2c3","publicKey":"ff00","keyGeneration":1,\
        "pairedDevices":1,"revokedDevices":0,"listenerAddress":"192.168.1.20:49221",\
        "ice":{"ufrag":"abc123","password":"a-short-term-stun-credential","candidates":[\
        {"type":"host","priority":2130706431,"foundation":"f1","component":1,\
        "protocol":"udp","address":"100.64.0.7:52000","relatedAddress":null,\
        "relatedPort":null,"tcpType":null,"expiresAt":99},\
        {"type":"srflx","priority":1694498815,"foundation":"f2","component":1,\
        "protocol":"udp","address":"203.0.113.9:52000","relatedAddress":"192.168.1.20",\
        "relatedPort":52000,"tcpType":null,"expiresAt":99}]}}
        """
        let status = try JSONDecoder().decode(RemoteAccessStatus.self, from: Data(payload.utf8))
        let ice = try XCTUnwrap(status.ice)
        XCTAssertEqual(ice.ufrag, "abc123")
        XCTAssertEqual(ice.candidates.count, 2)
        // A tunnel address is a host candidate like any other, which is what
        // makes a tailnet reachable without any special path.
        XCTAssertEqual(ice.candidates[0].address, "100.64.0.7:52000")

        // Republished unchanged apart from the lifetime: rewriting a priority
        // or foundation here would make the phone's pair ordering disagree
        // with the Mac's, while the helper's stamp (99, long past) is replaced
        // by this refresh's so an idle agent's presence keeps being accepted.
        let published = ice.candidates[1].published(expiresAt: 1_800_000_000)
        XCTAssertEqual(published.expiresAt, 1_800_000_000)
        XCTAssertEqual(published.type, "srflx")
        XCTAssertEqual(published.priority, 1_694_498_815)
        XCTAssertEqual(published.foundation, "f2")
        XCTAssertEqual(published.relatedAddress, "192.168.1.20")
        XCTAssertEqual(published.relatedPort, 52_000)
        XCTAssertNoThrow(try published.validatedForPublication())
    }

    func testAnOfferReachesTheHelperOnlyWhenItCouldProduceAConnection() throws {
        let full = ControlPlaneCandidate(
            address: "203.0.113.9:52000",
            expiresAt: 99,
            type: "srflx",
            priority: 1_694_498_815,
            foundation: "f2",
            component: 1,
            protocol: "udp"
        )
        // The service allows a bare address; the CLI needs the full ordering
        // metadata, so a candidate without it is dropped rather than sent.
        let bare = ControlPlaneCandidate(address: "203.0.113.10:52001", expiresAt: 99)

        let usable = try XCTUnwrap(
            RemoteRendezvousOfferDocument(
                offer(candidates: [full, bare], ufrag: "phoneufr", pwd: "phone-password")
            )
        )
        XCTAssertEqual(usable.candidates.count, 1)
        XCTAssertEqual(usable.candidates[0].address, "203.0.113.9:52000")

        // A peer with no agent of its own, or nothing to run checks against,
        // would only burn the helper's one gathered agent.
        XCTAssertNil(
            RemoteRendezvousOfferDocument(offer(candidates: [full], ufrag: nil, pwd: "pwd"))
        )
        XCTAssertNil(
            RemoteRendezvousOfferDocument(offer(candidates: [full], ufrag: "abc", pwd: nil))
        )
        XCTAssertNil(
            RemoteRendezvousOfferDocument(offer(candidates: [bare], ufrag: "abc", pwd: "pwd"))
        )
    }

    func testTheOfferDocumentMatchesTheCLIStdinContract() throws {
        let document = try XCTUnwrap(
            RemoteRendezvousOfferDocument(
                offer(
                    candidates: [
                        ControlPlaneCandidate(
                            address: "203.0.113.9:52000",
                            expiresAt: 99,
                            type: "srflx",
                            priority: 1,
                            foundation: "f2",
                            component: 1,
                            protocol: "udp"
                        )
                    ],
                    ufrag: "phoneufr",
                    pwd: "phone-password"
                )
            )
        )
        let encoded = try JSONSerialization.jsonObject(
            with: try JSONEncoder().encode(document)
        ) as? [String: Any]
        let body = try XCTUnwrap(encoded)
        XCTAssertEqual(body["requestId"] as? String, "r1")
        XCTAssertEqual(body["peerDeviceId"] as? String, "d1")
        XCTAssertEqual(body["iceUfrag"] as? String, "phoneufr")
        XCTAssertEqual(body["icePwd"] as? String, "phone-password")
        XCTAssertEqual(body["expiresAt"] as? UInt64, 99)
        // The peer's identity key is deliberately not carried: the Noise
        // handshake pins the paired identity, and the offer is transport only.
        XCTAssertNil(body["peerIdentityKey"])
        // The encoder omits nil rather than sending null, which is why the CLI
        // defaults the three optional candidate fields instead of requiring
        // them.
        let candidate = try XCTUnwrap((body["candidates"] as? [[String: Any]])?.first)
        XCTAssertEqual(
            Set(candidate.keys),
            ["type", "priority", "foundation", "component", "protocol", "address", "expiresAt"]
        )
    }

    @MainActor
    func testPresenceKeepsReflexiveCandidatesWhenAMacHasMoreInterfacesThanPresenceCarries() {
        let reflexive = gathered(type: "srflx", priority: 1_694_498_815, address: "203.0.113.9:1")
        let hosts = (0..<9).map { index in
            gathered(
                type: "host",
                priority: UInt32(2_130_706_431 - index),
                address: "192.168.1.\(index):1"
            )
        }
        // The reflexive candidate is gathered last and ranks lowest, which is
        // exactly the case a naive priority sort would drop.
        let selected = RemoteAccessController.presenceCandidates(from: hosts + [reflexive])

        // Four, because the listener now takes up to four of the eight places
        // presence has: one TCP candidate per interface it can be dialled on.
        XCTAssertEqual(selected.count, 4)
        XCTAssertEqual(selected.first?.address, "203.0.113.9:1")
        XCTAssertEqual(selected.dropFirst().map(\.address), (0..<3).map { "192.168.1.\($0):1" })

        // A Mac with room to spare publishes everything it gathered, in a
        // stable order across refreshes.
        let few = Array(hosts.prefix(2))
        XCTAssertEqual(
            RemoteAccessController.presenceCandidates(from: few).map(\.address),
            few.map(\.address)
        )
    }

    /// A CLI that predates these fields must still produce a decodable status:
    /// they are additions to a contract the desktop app polls constantly, and a
    /// hard requirement would turn a stale CLI into "remote access is broken".
    func testStatusReadsTheSleepAndRelayFieldsAndToleratesTheirAbsence() throws {
        let payload = """
        {"formatVersion":1,"enabled":true,"relayEnabled":false,"neverRelay":true,\
        "deviceId":"a1b2c3","publicKey":"ff00","keyGeneration":1,\
        "pairedDevices":1,"revokedDevices":0,"listenerAddress":"192.168.1.20:49221",\
        "activeConnections":2}
        """
        let status = try JSONDecoder().decode(RemoteAccessStatus.self, from: Data(payload.utf8))
        XCTAssertTrue(status.neverRelay)
        XCTAssertEqual(status.activeConnections, 2)
        XCTAssertTrue(status.hasLiveConnection)

        let older = """
        {"formatVersion":1,"enabled":true,"relayEnabled":true,\
        "deviceId":"a1b2c3","publicKey":"ff00","keyGeneration":1,\
        "pairedDevices":1,"revokedDevices":0,"listenerAddress":"192.168.1.20:49221"}
        """
        let legacy = try JSONDecoder().decode(RemoteAccessStatus.self, from: Data(older.utf8))
        XCTAssertFalse(legacy.neverRelay)
        XCTAssertEqual(legacy.activeConnections, 0)
        // No reported connections is not a reported connection, so a Mac
        // served by an older CLI sleeps on its ordinary schedule.
        XCTAssertFalse(legacy.hasLiveConnection)
    }

    /// The assertion is raised once and released once. A poll that repeats the
    /// same answer must not stack assertions, and a release must not leave one
    /// behind — a leaked one would keep the Mac awake with nothing connected.
    @MainActor
    func testSleepAssertionIsHeldOnlyWhileAPhoneIsConnected() {
        let preventer = RecordingSleepPreventer()
        let holder = SleepAssertionHolder(preventer: preventer)

        XCTAssertFalse(holder.isHeld)
        holder.apply(true)
        holder.apply(true)
        XCTAssertTrue(holder.isHeld)
        XCTAssertEqual(preventer.created, 1)

        holder.apply(false)
        holder.apply(false)
        XCTAssertFalse(holder.isHeld)
        XCTAssertEqual(preventer.released, 1)

        holder.apply(true)
        XCTAssertEqual(preventer.created, 2)
        XCTAssertTrue(holder.isHeld)
    }

    /// A Mac that refuses the relay outright publishes only the addresses a
    /// phone can reach without one. Withholding the reflexive candidates is the
    /// point: they are how a path through the internet is found, and that path
    /// is the one that ends in a relay when it cannot be found directly.
    @MainActor
    func testNeverRelayPublishesHostCandidatesOnly() {
        let candidates = [
            gathered(type: "host", priority: 2_130_706_431, address: "192.168.1.20:52000"),
            gathered(type: "host", priority: 2_130_706_430, address: "100.64.0.7:52000"),
            gathered(type: "srflx", priority: 1_694_498_815, address: "203.0.113.9:52000"),
        ]

        let permissive = RemoteAccessController.presenceCandidates(from: candidates)
        XCTAssertEqual(permissive.map(\.type), ["srflx", "host", "host"])

        let strict = RemoteAccessController.presenceCandidates(from: candidates, neverRelay: true)
        XCTAssertEqual(strict.map(\.address), ["192.168.1.20:52000", "100.64.0.7:52000"])
    }

    /// The helper binds `0.0.0.0`, so the address it reports names a port and
    /// no interface. The reachable addresses come from the agent's own host
    /// candidates — where the tailnet address already is — each republished as
    /// the same TCP listener on the same port.
    @MainActor
    func testTheListenerIsPublishedOnEveryInterfaceIncludingATailnetOne() throws {
        let gatheredHosts = [
            gathered(type: "host", priority: 2_130_706_431, address: "192.168.1.20:52000"),
            gathered(type: "host", priority: 2_130_706_430, address: "100.64.0.7:52001"),
            gathered(type: "host", priority: 2_130_706_429, address: "[fd7a:115c:a1e0::1]:52002"),
            gathered(type: "srflx", priority: 1_694_498_815, address: "203.0.113.9:52003"),
        ]
        let hosts = RemoteAccessController.interfaceHosts(from: gatheredHosts)
        XCTAssertEqual(hosts, ["192.168.1.20", "100.64.0.7", "fd7a:115c:a1e0::1"])

        let published = try ControlPlaneHost.listenerCandidates(
            "0.0.0.0:49221",
            interfaceHosts: hosts,
            now: Date(timeIntervalSince1970: 1_700_000_000)
        )
        // The unspecified bind itself is not published: a phone cannot dial it.
        XCTAssertEqual(
            published.map(\.address),
            ["192.168.1.20:49221", "100.64.0.7:49221", "[fd7a:115c:a1e0::1]:49221"]
        )
        XCTAssertTrue(published.allSatisfy { $0.protocol == "tcp" && $0.tcpType == "passive" })
        // Distinct bases must not share a foundation.
        XCTAssertEqual(Set(published.compactMap(\.foundation)).count, 3)
    }

    /// A specific bind is the owner having chosen an interface, so it keeps its
    /// place; a Mac with no reachable address at all is an error rather than a
    /// presence record nothing can act on.
    @MainActor
    func testListenerCandidatesKeepASpecificBindAndRefuseAnUnreachableOne() throws {
        let chosen = try ControlPlaneHost.listenerCandidates(
            "192.168.1.20:49221",
            interfaceHosts: ["100.64.0.7", "192.168.1.20"]
        )
        XCTAssertEqual(chosen.map(\.address), ["192.168.1.20:49221", "100.64.0.7:49221"])

        XCTAssertThrowsError(
            try ControlPlaneHost.listenerCandidates("0.0.0.0:49221", interfaceHosts: [])
        )
        XCTAssertThrowsError(
            try ControlPlaneHost.listenerCandidates("0.0.0.0:49221", interfaceHosts: ["127.0.0.1"])
        )
    }

    private func gathered(type: String, priority: UInt32, address: String) -> RemoteIceCandidate {
        RemoteIceCandidate(
            type: type,
            priority: priority,
            foundation: "f",
            component: 1,
            protocol: "udp",
            address: address,
            relatedAddress: nil,
            relatedPort: nil,
            tcpType: nil,
            expiresAt: 99
        )
    }

    private func offer(
        candidates: [ControlPlaneCandidate],
        ufrag: String?,
        pwd: String?
    ) -> ControlPlaneRendezvousOffer {
        ControlPlaneRendezvousOffer(
            requestID: "r1",
            peerDeviceID: "d1",
            peerIdentityKey: "ff00",
            candidates: candidates,
            iceUfrag: ufrag,
            icePwd: pwd,
            expiresAt: 99
        )
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
        // Paired before the CLI recorded a directory row: still a valid
        // device, simply one whose grant cannot be mirrored.
        XCTAssertNil(devices.first?.controlPlaneDeviceID)

        let recorded = try JSONDecoder().decode(
            [RemoteDevice].self,
            from: Data(#"[{"deviceId":"cc","name":"Phone","permission":"interact","revoked":false,"controlPlaneDeviceId":"dev_phone"}]"#.utf8)
        )
        XCTAssertEqual(recorded.first?.controlPlaneDeviceID, "dev_phone")

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

    /// The terminal is carried by `control` and by nothing below it, and
    /// taking it away returns the device to what it otherwise held rather than
    /// to a default.
    func testTheTerminalGrantIsControlAndWithdrawingItKeepsTheRest() {
        let observer = RemoteDevice(
            deviceID: "aa",
            name: "Phone",
            permission: .observe,
            revoked: false,
            controlPlaneDeviceID: nil
        )
        XCTAssertFalse(observer.allowsTerminal)
        XCTAssertEqual(observer.permissionWithoutTerminal, .observe)

        let interactor = RemoteDevice(
            deviceID: "bb",
            name: "Phone",
            permission: .interact,
            revoked: false,
            controlPlaneDeviceID: nil
        )
        XCTAssertFalse(interactor.allowsTerminal)
        XCTAssertEqual(interactor.permissionWithoutTerminal, .interact)

        let terminalHolder = RemoteDevice(
            deviceID: "cc",
            name: "Phone",
            permission: .control,
            revoked: false,
            controlPlaneDeviceID: "dev_phone"
        )
        XCTAssertTrue(terminalHolder.allowsTerminal)
        XCTAssertEqual(terminalHolder.permissionWithoutTerminal, .interact)
    }

    func testDesktopUsesControlLanguageForInteractiveAccess() {
        XCTAssertEqual(DevicePermission.observe.label, "Observe")
        XCTAssertEqual(DevicePermission.interact.label, "Control")
        XCTAssertEqual(DevicePermission.control.label, "Control + Terminal")
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

    /// The app re-encodes the bundle before writing it, so anything it does
    /// not decode is silently dropped from the exported file. A field run
    /// exports this bundle precisely for the path counters.
    func testExportedDiagnosticsKeepThePathCounters() throws {
        let document = """
        {"formatVersion":1,"remoteAccessEnabled":true,"relayEnabled":true,\
        "pairedDevices":2,"revokedDevices":0,"eventCounts":{"connection_opened":3},\
        "pathSelection":{"routes":{"lan":1,"direct_host":0,"direct_reflexive":1,\
        "relay":2,"unknown":0},"connections":4,"direct":2,"relay":2,\
        "iceAnswers":3,"iceAnswersConnected":2}}
        """
        let bundle = try JSONDecoder().decode(
            RemoteDiagnostics.self,
            from: Data(document.utf8)
        )
        XCTAssertEqual(bundle.pathSelection?.relay, 2)
        XCTAssertEqual(bundle.pathSelection?.relayShare, 0.5)
        XCTAssertEqual(bundle.pathSelection?.routes["direct_reflexive"], 1)

        let round = try JSONDecoder().decode(
            RemoteDiagnostics.self,
            from: JSONEncoder().encode(bundle)
        )
        XCTAssertEqual(round, bundle)
    }

    /// A bundle from a `latch` that predates the counters must still decode,
    /// and must not be readable as "nothing was relayed".
    func testDiagnosticsWithoutCountersDecodeAsUnmeasured() throws {
        let document = """
        {"formatVersion":1,"remoteAccessEnabled":true,"relayEnabled":true,\
        "pairedDevices":0,"revokedDevices":0,"eventCounts":{}}
        """
        let bundle = try JSONDecoder().decode(
            RemoteDiagnostics.self,
            from: Data(document.utf8)
        )
        XCTAssertNil(bundle.pathSelection)
    }

    func testPhaseReportsRunningOnlyWhenAListenerExists() {
        XCTAssertFalse(RemoteAccessPhase.off.isRunning)
        XCTAssertFalse(RemoteAccessPhase.starting.isRunning)
        XCTAssertFalse(RemoteAccessPhase.failed("boom").isRunning)
        XCTAssertTrue(RemoteAccessPhase.online(listener: "192.168.1.20:1").isRunning)
    }
}

/// Counts the assertions raised and released, so the holder's bookkeeping can
/// be asserted without asking IOKit to keep a test machine awake.
private final class RecordingSleepPreventer: SleepPreventing {
    private let lock = NSLock()
    private var nextID: UInt32 = 1
    private(set) var created = 0
    private(set) var released = 0

    func create(reason: String) -> UInt32? {
        lock.lock()
        defer { lock.unlock() }
        created += 1
        defer { nextID += 1 }
        return nextID
    }

    func release(_ assertion: UInt32) {
        lock.lock()
        defer { lock.unlock() }
        released += 1
    }
}
