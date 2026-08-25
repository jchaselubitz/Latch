import XCTest

@testable import LatchMobileKit

/// The phone now asks for relay servers alongside STUN on the first attempt,
/// which puts both requests on the critical path of every channel the
/// transport opens. These cover what that costs and when it is paid again.
final class IceConfigurationTests: XCTestCase {
    private let stun = IceServer(urls: ["stun:stun.example:3478"])

    private func turn(expiringAt expiresAt: UInt64) -> TurnCredentials {
        TurnCredentials(
            iceServers: [
                IceServer(
                    urls: ["turns:relay.example:5349?transport=tcp"],
                    username: "device",
                    credential: "secret"
                )
            ],
            expiresAt: expiresAt
        )
    }

    func testStunIsFetchedOncePerWindowAndAgainAfterIt() async throws {
        let clock = TestClock(Date(timeIntervalSince1970: 1_000))
        let configuration = IceConfiguration(now: clock.read)
        var fetches = 0
        let fetch: @Sendable () async throws -> [IceServer] = {
            fetches += 1
            return [self.stun]
        }

        _ = try await configuration.stun(fetch)
        _ = try await configuration.stun(fetch)
        XCTAssertEqual(fetches, 1, "a second channel re-read static STUN configuration")

        clock.advance(by: 301)
        _ = try await configuration.stun(fetch)
        XCTAssertEqual(fetches, 2, "STUN configuration was held past its window")
    }

    func testAnUnexpiredCredentialIsReusedRatherThanMintedAgain() async {
        let clock = TestClock(Date(timeIntervalSince1970: 1_000))
        let configuration = IceConfiguration(now: clock.read)
        var issued = 0
        let fetch: @Sendable () async throws -> TurnCredentials = {
            issued += 1
            return self.turn(expiringAt: 1_120)
        }

        let first = await configuration.relay(fetch)
        XCTAssertTrue(first.servers.contains(where: \.isTurn))
        XCTAssertFalse(first.refused)

        _ = await configuration.relay(fetch)
        XCTAssertEqual(issued, 1, "a credential valid for another two minutes was minted twice")

        // The margin discards the credential before its stated expiry rather
        // than at it, so a channel opened at the boundary does not gather
        // against a server that stops answering mid-allocation.
        clock.advance(by: 106)
        _ = await configuration.relay(fetch)
        XCTAssertEqual(issued, 2, "an expiring credential was reused inside its margin")
    }

    func testARefusalIsReportedAsSuchAndNeverCached() async {
        let configuration = IceConfiguration(now: { Date(timeIntervalSince1970: 1_000) })
        var calls = 0
        let refuse: @Sendable () async throws -> TurnCredentials = {
            calls += 1
            throw ControlPlaneError.relayDisabled("")
        }

        let refused = await configuration.relay(refuse)
        XCTAssertTrue(refused.servers.isEmpty)
        XCTAssertTrue(refused.refused, "the account kill switch was not distinguished from an outage")

        _ = await configuration.relay(refuse)
        XCTAssertEqual(calls, 2, "a refusal was cached, so re-enabling relay would not take effect")
    }

    func testAnOutageLeavesTheAttemptDirectOnlyWithoutClaimingARefusal() async {
        let configuration = IceConfiguration(now: { Date(timeIntervalSince1970: 1_000) })
        let unavailable = await configuration.relay {
            throw ControlPlaneError.http(status: 503, path: "/v1/turn-credentials", reason: "relay_unavailable")
        }
        XCTAssertTrue(unavailable.servers.isEmpty)
        XCTAssertFalse(
            unavailable.refused,
            "a service outage was reported as a refusal, which suppresses the relay retry"
        )
    }
}

/// A movable clock. `IceConfiguration` is the only thing reading it, and it is
/// read from inside an actor, so the lock is what makes that legal rather than
/// a concurrency claim about the test.
private final class TestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current: Date

    init(_ start: Date) {
        current = start
    }

    var read: @Sendable () -> Date {
        { [self] in
            lock.lock()
            defer { lock.unlock() }
            return current
        }
    }

    func advance(by seconds: TimeInterval) {
        lock.lock()
        current += seconds
        lock.unlock()
    }
}
