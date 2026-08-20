import XCTest

@testable import LatchMobileKit

private final class FactoryCallCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0

    func increment() {
        lock.lock()
        value += 1
        lock.unlock()
    }

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

@MainActor
final class AppModelTests: XCTestCase {
    private let discovery = """
    {"protocolVersion":1,"productVersion":"1.0.0",
     "endpoints":{"sessions":true,"sessionCapabilities":true,"terminal":true,
                  "events":true,"send":true},
     "features":{"idempotencyKeys":true,"readOnlyTerminal":true},
     "gatewayInstanceId":"gw-paired"}
    """

    override func setUp() {
        super.setUp()
        StubProtocol.reset()
    }

    private func pairedRecord(permission: DevicePermission = .interact, revoked: Bool = false) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "dev_phone",
            name: "Test iPhone",
            devicePublicKey: String(repeating: "a", count: 64),
            mac: PairedMac(
                deviceId: "dev_mac",
                publicKey: String(repeating: "b", count: 64),
                name: "Test Mac"
            ),
            permission: permission,
            revoked: revoked,
            phrase: "cedar orbit"
        )
    }

    private func pairedGatewayFactory() -> @Sendable (PairedDeviceRecord) async throws -> LatchGateway {
        { _ in
            let link = try GatewayLink(address: "http://127.0.0.1:8787", token: "")
            return LatchGateway(link: link, session: StubProtocol.session())
        }
    }

    private func model() -> AppModel {
        AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { link in
                LatchGateway(link: link, session: StubProtocol.session())
            },
            pairedGatewayFactory: pairedGatewayFactory()
        )
    }

    private func stubReachableMac() {
        StubProtocol.stub(path: "/v1/capabilities", body: discovery)
        StubProtocol.stub(
            path: "/v1/sessions",
            body: """
            {"sessions":[{"id":"ses_1","name":"work","state":"running","cwd":"/work",
            "command_label":"latch","created_at":"2026-08-16T12:00:00Z"}]}
            """
        )
    }

    func testPairedRouteDiscoversAndListsSessionsWithoutAManualLink() async {
        stubReachableMac()
        let model = model()

        await model.connectPairedDevice(pairedRecord())

        guard case .linked = model.linkState else {
            return XCTFail("paired connection failed: \(model.linkState)")
        }
        XCTAssertEqual(model.linkSource, .paired)
        XCTAssertEqual(model.link?.token, "", "the gateway credential stays on the Mac")
        XCTAssertEqual(model.sessions.map(\.id), ["ses_1"])
        XCTAssertNil(StubProtocol.requests.first?.headers["Authorization"])
    }

    func testObserveOnlyPairingHidesComposerAndInteractionControls() async {
        stubReachableMac()
        let model = model()

        await model.connectPairedDevice(pairedRecord(permission: .observe))

        XCTAssertTrue(model.surface.chat)
        XCTAssertFalse(model.surface.composer)
        XCTAssertFalse(model.surface.interactionControls)
    }

    func testManualLinkRemainsTheSelectedRouteAfterPairing() async throws {
        stubReachableMac()
        let model = model()
        await model.link(address: "http://127.0.0.1:8787", token: "manual-token")
        XCTAssertEqual(model.linkSource, .manual)

        await model.connectPairedDevice(pairedRecord())

        XCTAssertEqual(model.linkSource, .manual)
        XCTAssertEqual(model.link?.token, "manual-token")
    }

    func testRevokedPairingDropsThePairedRoute() async {
        stubReachableMac()
        let model = model()
        await model.connectPairedDevice(pairedRecord())
        XCTAssertEqual(model.linkSource, .paired)

        await model.connectPairedDevice(pairedRecord(revoked: true))

        XCTAssertEqual(model.linkState, .unlinked)
        XCTAssertNil(model.gateway)
    }

    func testSuspendedPairedRouteIsRebuiltAndRediscoveredOnResume() async {
        stubReachableMac()
        let factoryCalls = FactoryCallCounter()
        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { link in LatchGateway(link: link, session: StubProtocol.session()) },
            pairedGatewayFactory: { _ in
                factoryCalls.increment()
                let link = try GatewayLink(address: "http://127.0.0.1:8787", token: "")
                return LatchGateway(link: link, session: StubProtocol.session())
            }
        )

        await model.connectPairedDevice(pairedRecord())
        XCTAssertEqual(factoryCalls.count, 1)
        XCTAssertEqual(model.linkSource, .paired)

        model.suspendPairedTransport()

        XCTAssertEqual(model.linkState, .unlinked)
        XCTAssertNil(model.gateway)
        XCTAssertNil(model.link)
        XCTAssertNil(model.linkSource)

        await model.reconnectPairedTransport()

        guard case .linked = model.linkState else {
            return XCTFail("paired reconnection failed: \(model.linkState)")
        }
        XCTAssertEqual(factoryCalls.count, 2)
        XCTAssertEqual(model.linkSource, .paired)
        XCTAssertEqual(model.sessions.map(\.id), ["ses_1"])
    }

    func testSuspendingPairedRouteNeverDisconnectsManualLink() async throws {
        stubReachableMac()
        let model = model()
        await model.link(address: "http://127.0.0.1:8787", token: "manual-token")

        model.suspendPairedTransport()

        XCTAssertEqual(model.linkSource, .manual)
        XCTAssertNotNil(model.gateway)
    }

    func testSuspensionCannotResurrectAnInFlightPairedRoute() async throws {
        stubReachableMac()
        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { link in LatchGateway(link: link, session: StubProtocol.session()) },
            pairedGatewayFactory: { _ in
                try await Task.sleep(nanoseconds: 20_000_000)
                let link = try GatewayLink(address: "http://127.0.0.1:8787", token: "")
                return LatchGateway(link: link, session: StubProtocol.session())
            }
        )

        let connecting = Task { await model.connectPairedDevice(self.pairedRecord()) }
        await Task.yield()
        model.suspendPairedTransport()
        await connecting.value

        XCTAssertEqual(model.linkState, .unlinked)
        XCTAssertNil(model.gateway)
        XCTAssertNil(model.linkSource)
    }

    /// Discovery answering with a newer major, as a Mac that has crossed a
    /// protocol boundary this build predates.
    private let futureDiscovery = """
    {"protocolVersion":2,"productVersion":"2.0.0",
     "endpoints":{"sessions":true,"sessionCapabilities":true,"terminal":true,
                  "events":true,"send":true},
     "features":{"idempotencyKeys":true,"readOnlyTerminal":true},
     "gatewayInstanceId":"gw-future"}
    """

    func testANewerGatewayAsksThePersonToUpdateThisPhone() async throws {
        // This is the shape of the coordinated v2 release seen from a phone
        // that has not been updated yet. The Mac is reachable and healthy, so
        // reporting "cannot reach that computer" would send someone to debug
        // their network for a problem the App Store fixes.
        StubProtocol.stub(path: "/v1/capabilities", body: futureDiscovery)
        let saved = try GatewayLink(address: "http://127.0.0.1:8787", token: "token")
        let storage = MemoryLinkStorage(link: saved)
        let model = AppModel(
            storage: storage,
            sessionFactory: { link in LatchGateway(link: link, session: StubProtocol.session()) },
            pairedGatewayFactory: pairedGatewayFactory()
        )

        await model.restore()

        guard case .incompatible(let mismatch) = model.linkState else {
            return XCTFail("a newer gateway must not read as a connection failure: \(model.linkState)")
        }
        XCTAssertEqual(mismatch, .updatePhone(reported: 2, supported: 1))
        XCTAssertNotNil(
            try storage.load(),
            "the saved computer is still correct; only this build is behind"
        )
        XCTAssertEqual(
            model.surface,
            SessionSurface(chat: false, composer: false, interactionControls: false),
            "nothing may be offered across a protocol major"
        )
    }

    func testAPairedNewerGatewayReportsTheSameThingAndKeepsThePairing() async {
        // The paired path builds its own link, so it has a second copy of the
        // failure handling. It has to classify a mismatch the same way, or the
        // message a person sees depends on which route reached the Mac.
        StubProtocol.stub(path: "/v1/capabilities", body: futureDiscovery)
        let model = model()

        await model.connectPairedDevice(pairedRecord())

        guard case .incompatible(let mismatch) = model.linkState else {
            return XCTFail("paired route must report the mismatch too: \(model.linkState)")
        }
        XCTAssertEqual(mismatch, .updatePhone(reported: 2, supported: 1))

        // Updating the app is the remedy, so the pairing must survive to be
        // reconnected afterwards rather than being dropped as a bad route.
        StubProtocol.reset()
        stubReachableMac()
        await model.reconnectPairedTransport()

        guard case .linked = model.linkState else {
            return XCTFail("the pairing should reconnect once the versions agree: \(model.linkState)")
        }
        XCTAssertEqual(model.linkSource, .paired)
    }

    func testASavedControlPlaneAddressDoesNotStayLinkedOrBlockPairing() async throws {
        let controlPlane = #"{"error":"not_found","reason":"no such resource"}"#
        StubProtocol.stub(path: "/v1/capabilities", status: 404, body: controlPlane)
        let saved = try GatewayLink(
            address: "https://latch-production-7e52.up.railway.app",
            token: "not-a-gateway-token"
        )
        let storage = MemoryLinkStorage(link: saved)
        let model = AppModel(
            storage: storage,
            sessionFactory: { link in LatchGateway(link: link, session: StubProtocol.session()) },
            pairedGatewayFactory: pairedGatewayFactory()
        )

        await model.restore()

        guard case .failed(let reason) = model.linkState else {
            return XCTFail("control-plane URL must not look like a linked gateway: \(model.linkState)")
        }
        XCTAssertTrue(reason.contains("control plane"), reason)
        XCTAssertNil(try storage.load(), "a control-plane URL must not remain the saved computer")

        StubProtocol.reset()
        stubReachableMac()
        await model.connectPairedDevice(pairedRecord())

        guard case .linked = model.linkState else {
            return XCTFail("pairing should connect after a control-plane link is rejected: \(model.linkState)")
        }
        XCTAssertEqual(model.linkSource, .paired)
        XCTAssertEqual(model.sessions.map(\.id), ["ses_1"])
    }
}
