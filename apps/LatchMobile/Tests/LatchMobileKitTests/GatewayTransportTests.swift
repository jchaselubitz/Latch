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
