import XCTest

@testable import LatchMobileKit

final class GatewayTransportTests: XCTestCase {
    func testGatewayRetainsItsCapabilityTransportForItsFullLifetime() async {
        var transport: RetainedGatewayTransport? = RetainedGatewayTransport()
        weak let retained = transport
        var gateway: LatchGateway? = LatchGateway(transport: transport!)
        transport = nil

        XCTAssertNotNil(retained)
        _ = await gateway?.gateway
        gateway = nil
        for _ in 0..<10 where retained != nil { await Task.yield() }
        XCTAssertNil(retained)
    }

    func testListenerConnectionsWaitUntilTheTransportHandlerIsInstalled() {
        let router = DeferredConnectionHandler<Int>()
        let received = LockedConnectionValues()

        router.receive(1)
        router.install { received.append($0) }
        router.receive(2)

        XCTAssertEqual(received.values, [1, 2])
    }

    func testLoopbackCapabilityIsRequiredAndStrippedBeforeForwarding() throws {
        var validator = TunnelRequestValidator(expectedCapability: "local-secret")
        XCTAssertNil(try validator.append(Data(
            "GET /v2/capabilities HTTP/1.1\r\nHost: loop".utf8
        )))
        let first = try XCTUnwrap(try validator.append(Data(
            "back\r\nAuthorization: Bearer local-secret\r\n\r\nbody".utf8
        )))
        XCTAssertEqual(
            String(data: first, encoding: .utf8),
            "GET /v2/capabilities HTTP/1.1\r\nHost: loopback\r\n\r\nbody"
        )
        XCTAssertEqual(
            try validator.append(Data("later request bytes".utf8)),
            Data("later request bytes".utf8)
        )
    }

    func testLoopbackRejectsMissingWrongDuplicateAndProxyCredentials() {
        let requests = [
            "GET / HTTP/1.1\r\nHost: loopback\r\n\r\n",
            "GET / HTTP/1.1\r\nAuthorization: Bearer wrong\r\n\r\n",
            "GET / HTTP/1.1\r\nAuthorization: Bearer local-secret\r\nAuthorization: Bearer local-secret\r\n\r\n",
            "GET / HTTP/1.1\r\nAuthorization: Bearer local-secret\r\npRoXy-AuThOrIzAtIoN: Basic secret\r\n\r\n"
        ]
        for request in requests {
            var validator = TunnelRequestValidator(expectedCapability: "local-secret")
            XCTAssertThrowsError(try validator.append(Data(request.utf8))) { error in
                XCTAssertEqual(error as? RemoteLinkTransportError, .invalidCapability)
            }
        }
    }

    private static let macPin = String(repeating: "ab", count: 32)

    private func txt(_ lanAddrs: String, identity: String = macPin) -> [String: String] {
        [
            "identityKey": identity,
            "linkVersion": "1",
            "lanHost": "latch-abababababab-cdcdcdcdcdcd.local",
            "lanPort": "51234",
            "lanAddrs": lanAddrs,
        ]
    }

    func testBonjourRecordKeepsOnlyPrivateAddressLiterals() {
        let targets = RemoteLinkLanTarget.targets(
            fromTXT: txt("192.168.1.20, 8.8.8.8,evil.example.com,10.0.0.5,fd12:3456::1,2001:4860::8888,fe80::1"),
            matching: Self.macPin.uppercased()
        )
        XCTAssertEqual(targets.map(\.host), ["192.168.1.20", "10.0.0.5", "fd12:3456::1", "fe80::1"])
        XCTAssertTrue(targets.allSatisfy { $0.port == 51234 })
    }

    func testBonjourRecordWithPublicAddressOrHostnameYieldsNoLanTargets() {
        for lanAddrs in ["8.8.8.8", "evil.example.com", "latch-mac.local", "", "2001:4860::8888", "127.0.0.1", "100.64.0.1"] {
            XCTAssertEqual(
                RemoteLinkLanTarget.targets(fromTXT: txt(lanAddrs), matching: Self.macPin), [],
                "\(lanAddrs) should yield no LAN target"
            )
        }
        // The published `.local` name is never a fallback target.
        XCTAssertEqual(RemoteLinkLanTarget.targets(fromTXT: txt(""), matching: Self.macPin), [])
        // A record for another Mac is ignored whatever it publishes.
        XCTAssertEqual(
            RemoteLinkLanTarget.targets(
                fromTXT: txt("192.168.1.20", identity: String(repeating: "cd", count: 32)),
                matching: Self.macPin
            ),
            []
        )
    }

    func testSharedNetworkLiteralMatchesTheFfiRanges() {
        for host in ["10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.0.1", "169.254.1.1", "fc00::1", "fdff::1", "fe80::1", "febf::1"] {
            XCTAssertTrue(RemoteLinkLanTarget.isSharedNetworkLiteral(host), host)
        }
        for host in ["172.15.0.1", "172.32.0.1", "11.0.0.1", "0.0.0.0", "255.255.255.255", "::1", "fec0::1", "::ffff:192.168.1.1", "fe80::1%en0", "192.168.1.1:22", " 10.0.0.1"] {
            XCTAssertFalse(RemoteLinkLanTarget.isSharedNetworkLiteral(host), host)
        }
    }

    private struct CandidateRefused: Error {}

    func testFailedCandidateDoesNotAbandonTheRemainingLanTargets() async {
        let targets = [
            RemoteLinkLanTarget(host: "192.168.1.66", port: 51234),
            RemoteLinkLanTarget(host: "192.168.1.20", port: 51234),
        ]
        var attempted: [String] = []
        var failures = 0
        let link = await RemoteLinkLanTarget.connectFirst(
            targets, budget: .milliseconds(1500), onFailure: { _ in failures += 1 }
        ) { target -> String in
            attempted.append(target.host)
            // The first host answers but fails Noise pinning, as a spoofed
            // Bonjour record would.
            if target.host == "192.168.1.66" { throw CandidateRefused() }
            return target.host
        }
        XCTAssertEqual(link, "192.168.1.20")
        XCTAssertEqual(attempted, ["192.168.1.66", "192.168.1.20"])
        XCTAssertEqual(failures, 1)
    }

    func testLanCandidatesStopOnceTheBudgetIsSpent() async {
        let targets = (1...3).map { RemoteLinkLanTarget(host: "192.168.1.\($0)", port: 51234) }
        var attempted = 0
        let link: String? = await RemoteLinkLanTarget.connectFirst(targets, budget: .milliseconds(100)) { _ in
            attempted += 1
            try await Task.sleep(for: .milliseconds(150))
            throw CandidateRefused()
        }
        XCTAssertNil(link)
        XCTAssertEqual(attempted, 1)
    }

    func testRemoteLinkBonjourTypeIsStable() {
        XCTAssertEqual(BonjourMacDiscovery.serviceType, "_latch-remote._tcp")
    }
}

private final class RetainedGatewayTransport: GatewayTransport, @unchecked Sendable {
    let gatewayLink = GatewayLink(
        url: URL(string: "http://127.0.0.1:1")!,
        token: "retained-capability"
    )
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
