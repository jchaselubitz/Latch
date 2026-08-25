import XCTest

@testable import LatchMobileKit

final class GatewayTransportTests: XCTestCase {
    func testListenerConnectionsWaitUntilTheTransportHandlerIsInstalled() {
        let router = DeferredConnectionHandler<Int>()
        let received = LockedConnectionValues()

        router.receive(1)
        router.install { received.append($0) }
        router.receive(2)

        XCTAssertEqual(received.values, [1, 2])
    }

    func testBonjourIdentityHintIsOptionalButAnExplicitMismatchIsSkipped() {
        let pin = String(repeating: "a", count: 64)

        XCTAssertTrue(
            BonjourMacDiscovery.shouldAttempt(advertisedIdentityKey: pin, normalizedPin: pin)
        )
        XCTAssertTrue(
            BonjourMacDiscovery.shouldAttempt(advertisedIdentityKey: nil, normalizedPin: pin),
            "Noise authenticates a result whose TXT metadata has not arrived yet"
        )
        XCTAssertFalse(
            BonjourMacDiscovery.shouldAttempt(
                advertisedIdentityKey: String(repeating: "b", count: 64),
                normalizedPin: pin
            )
        )
    }

    func testRelayIsAllowedFromTheFirstAttemptButNeedsARelayServer() async throws {
        let policy = RemoteTransportPolicy()
        let stun = IceServer(urls: ["stun:stun.example:3478"])
        let turn = IceServer(
            urls: ["stun:stun.example:3478", "turns:relay.example:5349?transport=tcp"],
            username: "device",
            credential: "secret"
        )
        XCTAssertFalse(stun.isTurn)
        XCTAssertTrue(turn.isTurn)
        do {
            try await policy.authorizeRelayAttempt(servers: [stun])
            XCTFail("a STUN-only list was accepted as a relay attempt")
        } catch {
            XCTAssertEqual(error as? RemoteTransportPolicyError, .missingTurnServer)
        }
        try await policy.authorizeRelayAttempt(servers: [stun, turn])
    }

    func testPathChangeRequiresCapabilityRediscoveryOnlyOnChange() async {
        let policy = RemoteTransportPolicy()
        let firstRelay = await policy.recordSelectedPath(.relay)
        let sameRelay = await policy.recordSelectedPath(.relay)
        let changedToDirect = await policy.recordSelectedPath(.direct)
        let sameDirect = await policy.recordSelectedPath(.direct)
        XCTAssertFalse(firstRelay)
        XCTAssertFalse(sameRelay)
        XCTAssertTrue(changedToDirect)
        XCTAssertFalse(sameDirect)
    }

    func testManualTransportPreservesTheEnteredLink() throws {
        let link = try GatewayLink(address: "https://gateway.example", token: "manual-token")
        let transport = HTTPSGatewayTransport(link: link)
        XCTAssertEqual(transport.gatewayLink, link)
    }

    func testTunnelRequestWaitsForACompleteHeaderThenForwardsItOnce() throws {
        var validator = TunnelRequestValidator()
        XCTAssertNil(try validator.append(Data("GET /v2/capabilities HTTP/1.1\r\nHost: loop".utf8)))
        let first = try XCTUnwrap(try validator.append(Data("back\r\n\r\nbody".utf8)))
        XCTAssertEqual(String(data: first, encoding: .utf8), "GET /v2/capabilities HTTP/1.1\r\nHost: loopback\r\n\r\nbody")
        XCTAssertEqual(
            try validator.append(Data("later request bytes".utf8)),
            Data("later request bytes".utf8),
            "a completed loopback connection does not buffer a second request"
        )
    }

    func testTunnelRejectsAuthorizationBeforeOpeningANoiseSession() throws {
        var validator = TunnelRequestValidator()
        XCTAssertThrowsError(
            try validator.append(Data("GET / HTTP/1.1\r\nAuthorization: Bearer secret\r\n\r\n".utf8))
        ) { error in
            XCTAssertEqual(error as? NoiseTunnelError, .callerSuppliedCredential)
        }
    }

    func testTunnelRejectsProxyAuthorizationCaseInsensitively() throws {
        var validator = TunnelRequestValidator()
        XCTAssertThrowsError(
            try validator.append(Data("GET / HTTP/1.1\r\npRoXy-AuThOrIzAtIoN: Basic secret\r\n\r\n".utf8))
        ) { error in
            XCTAssertEqual(error as? NoiseTunnelError, .callerSuppliedCredential)
        }
    }
}

private final class LockedConnectionValues: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [Int] = []

    var values: [Int] {
        lock.lock()
        defer { lock.unlock() }
        return storage
    }

    func append(_ value: Int) {
        lock.lock()
        storage.append(value)
        lock.unlock()
    }
}
