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
}
