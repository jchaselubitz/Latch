import XCTest

@testable import LatchMobileKit

/// Presence, rendezvous, and TURN credential calls over the same stub
/// transport pairing uses. What matters is the wire shape and that each
/// failure becomes a sentence a person can act on.
final class SignalingTests: XCTestCase {
    private let base = URL(string: "https://control.example")!
    private let token = "token-1"
    private let macId = "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    private let phoneId = "dev_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    private let macKey = String(repeating: "1", count: 64)
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    override func setUp() {
        super.setUp()
        StubProtocol.reset()
    }

    override func tearDown() {
        StubProtocol.reset()
        super.tearDown()
    }

    private func client() -> HTTPControlPlaneClient {
        HTTPControlPlaneClient(baseURL: base, session: StubProtocol.session())
    }

    private func candidate(
        address: String = "203.0.113.7:41234",
        lifetime: UInt64 = 60
    ) throws -> TransportCandidate {
        try TransportCandidate.shortLived(address: address, lifetime: lifetime)
    }

    private func iceCandidate(
        address: String = "203.0.113.7:41234",
        protocol proto: String = "udp"
    ) throws -> TransportCandidate {
        try TransportCandidate(
            address: address,
            expiresAt: UInt64(Date().timeIntervalSince1970) + 60,
            type: "srflx",
            priority: 1_694_498_175,
            foundation: "aB-1",
            component: 1,
            protocol: proto,
            relatedAddress: "10.0.0.4",
            relatedPort: 40_000,
            tcpType: proto == "tcp" ? "passive" : nil
        ).validated(maxLifetime: 60)
    }

    private func record(macDeviceId: String? = "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: phoneId,
            name: "Jake's iPhone",
            devicePublicKey: String(repeating: "2", count: 64),
            mac: PairedMac(deviceId: macDeviceId, publicKey: macKey, name: "Studio Mac"),
            permission: .interact,
            phrase: "sable-apple-maple-garnet-maple-flint",
            pairedAt: now,
            controlPlane: base,
            accessToken: token
        )
    }

    // MARK: - Presence

    func testPresenceOfTheMacCarriesTheAccessTokenAndReportsOnlineCandidates() async throws {
        StubProtocol.stub(
            path: "/v1/presence/\(macId)",
            body: """
            {
              "deviceId": "\(macId)",
              "online": true,
              "identityKey": "\(macKey)",
              "candidates": [{"address": "192.168.1.20:49221", "expiresAt": 1700000060, "type": "host", "priority": 123, "foundation": "mac1", "component": 1, "protocol": "tcp", "tcpType": "passive"}],
              "iceUfrag": "macUfrag",
              "icePwd": "mac-password-1234567890",
              "expiresAt": 1700000060
            }
            """
        )

        let presence = try await client().macPresence(for: record())
        XCTAssertTrue(presence.online)
        XCTAssertEqual(presence.identityKey, macKey)
        XCTAssertEqual(presence.candidates.map(\.address), ["192.168.1.20:49221"])
        XCTAssertEqual(presence.candidates.first?.type, "host")
        XCTAssertEqual(presence.candidates.first?.tcpType, "passive")
        XCTAssertEqual(presence.iceUfrag, "macUfrag")
        XCTAssertEqual(presence.icePwd, "mac-password-1234567890")

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.path, "/v1/presence/\(macId)")
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
        XCTAssertEqual(request.headers["Accept"], "application/json")
    }

    func testOfflinePresenceIsAStateNotAnError() async throws {
        StubProtocol.stub(
            path: "/v1/presence/\(macId)",
            body: """
            {"deviceId": "\(macId)", "online": false, "candidates": []}
            """
        )
        let presence = try await client().presence(deviceId: macId, accessToken: token)
        XCTAssertFalse(presence.online)
        XCTAssertTrue(presence.candidates.isEmpty)
        XCTAssertNil(presence.identityKey)
    }

    func testPublishPresenceSendsOnlyValidatedCandidatesAndReadsTheServiceTTL() async throws {
        StubProtocol.stub(
            path: "/v1/presence",
            body: """
            {"deviceId": "\(phoneId)", "expiresAt": 1700000090, "ttlSeconds": 90}
            """
        )
        let publishedCandidate = try candidate(address: "198.51.100.4:52111", lifetime: 90)
        let published = try await client().publishPresence(
            candidates: [publishedCandidate],
            iceUfrag: "phoneUfrag",
            icePwd: String(repeating: "p", count: 22),
            accessToken: token
        )
        XCTAssertEqual(published.ttlSeconds, 90)
        XCTAssertEqual(published.deviceId, phoneId)

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.path, "/v1/presence")
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
        XCTAssertTrue(request.body.contains("198.51.100.4:52111"))
        XCTAssertTrue(request.body.contains("\(publishedCandidate.expiresAt)"))
        XCTAssertTrue(request.body.contains("\"iceUfrag\":\"phoneUfrag\""))
        XCTAssertTrue(request.body.contains("\"icePwd\":"))
        XCTAssertFalse(request.body.contains("loopback"))
    }

    func testClearPresenceDeletesWithTheBearerAndAcceptsABody() async throws {
        StubProtocol.stub(
            path: "/v1/presence",
            body: #"{"deviceId":"dev_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","published":false}"#
        )
        try await client().clearPresence(accessToken: token)
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }

    // MARK: - Rendezvous

    func testOfferRendezvousReadsTheMacCandidatesPermissionAndClaimedKey() async throws {
        StubProtocol.stub(
            path: "/v1/rendezvous",
            body: """
            {
              "requestId": "request-0001",
              "peerDeviceId": "\(macId)",
              "peerIdentityKey": "\(macKey)",
              "permission": "interact",
              "candidates": [{"address": "192.168.1.20:49221", "expiresAt": 1700000060, "type": "host", "priority": 123, "foundation": "mac1", "component": 1, "protocol": "tcp", "tcpType": "passive"}],
              "iceUfrag": "macUfrag",
              "icePwd": "mac-password-1234567890",
              "expiresAt": 1700000060
            }
            """
        )

        let answer = try await client().offerRendezvous(
            for: record(),
            candidates: [try iceCandidate(address: "[2001:db8::1]:41999")],
            iceUfrag: "phoneUfrag",
            icePwd: String(repeating: "p", count: 22),
            requestId: "request-0001"
        )
        XCTAssertEqual(answer.peerDeviceId, macId)
        XCTAssertEqual(answer.permission, .interact)
        XCTAssertEqual(answer.candidates.map(\.address), ["192.168.1.20:49221"])
        XCTAssertEqual(answer.candidates.first?.foundation, "mac1")
        XCTAssertEqual(answer.iceUfrag, "macUfrag")
        XCTAssertEqual(answer.icePwd, "mac-password-1234567890")
        XCTAssertTrue(answer.matchesPinnedKey(macKey))
        XCTAssertFalse(answer.matchesPinnedKey(String(repeating: "f", count: 64)))

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertTrue(request.body.contains("\"targetDeviceId\":\"\(macId)\""))
        XCTAssertTrue(request.body.contains("request-0001"))
        XCTAssertTrue(request.body.contains("[2001:db8::1]:41999"))
        XCTAssertTrue(request.body.contains("\"type\":\"srflx\""))
        XCTAssertTrue(request.body.contains("\"relatedAddress\":\"10.0.0.4\""))
        XCTAssertTrue(request.body.contains("\"iceUfrag\":\"phoneUfrag\""))
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }

    func testUnknownRendezvousGrantDegradesToObserve() async throws {
        StubProtocol.stub(
            path: "/v1/rendezvous",
            body: """
            {
              "requestId": "request-0001",
              "peerDeviceId": "\(macId)",
              "peerIdentityKey": "\(macKey)",
              "permission": "manage",
              "candidates": [{"address": "192.168.1.20:49221", "expiresAt": 1700000060}],
              "expiresAt": 1700000060
            }
            """
        )
        let answer = try await client().offerRendezvous(
            targetDeviceId: macId,
            candidates: [try candidate()],
            accessToken: token,
            requestId: "request-0001",
            expiresAt: nil
        )
        XCTAssertEqual(answer.permission, .observe)
        let updated = record().updating(permission: answer.permission)
        XCTAssertEqual(updated.permission, .observe)
    }

    func testTargetOfflineBecomesAReachableMacSentenceAndIsRetryable() async throws {
        StubProtocol.stub(
            path: "/v1/rendezvous",
            status: 409,
            body: #"{"error":"target_offline","reason":"target device has no current presence"}"#
        )
        do {
            _ = try await client().offerRendezvous(
                targetDeviceId: macId,
                candidates: [try candidate()],
                accessToken: token,
                requestId: "request-0001",
                expiresAt: nil
            )
            XCTFail("expected macNotReachable")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(
                error,
                .macNotReachable("target device has no current presence")
            )
            XCTAssertTrue(error.isRetryable)
            XCTAssertTrue(error.message.hasPrefix("Your Mac is not reachable right now"))
            XCTAssertTrue(error.message.contains("target device has no current presence"))
        }
    }

    func testCollectOffersReadsTheInboundSetOnce() async throws {
        StubProtocol.stub(
            path: "/v1/rendezvous",
            body: """
            {
              "offers": [{
                "requestId": "request-0001",
                "peerDeviceId": "\(phoneId)",
                "peerIdentityKey": "\(String(repeating: "2", count: 64))",
                "candidates": [{"address": "10.0.0.4:4000", "expiresAt": 1700000050}],
                "iceUfrag": "phoneUfrag",
                "icePwd": "phone-password-12345678",
                "expiresAt": 1700000050
              }]
            }
            """
        )
        let offers = try await client().collectOffers(accessToken: token)
        XCTAssertEqual(offers.map(\.requestId), ["request-0001"])
        XCTAssertEqual(offers.first?.peerDeviceId, phoneId)
        XCTAssertEqual(offers.first?.iceUfrag, "phoneUfrag")
        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }

    // MARK: - TURN

    func testIceServersAreStunOnlyAndDeviceAuthenticated() async throws {
        StubProtocol.stub(
            path: "/v1/ice-servers",
            body: #"{"iceServers":[{"urls":["stun:stun.cloudflare.com:3478"]}]}"#
        )

        let servers = try await client().iceServers(for: record())
        XCTAssertEqual(servers.map(\.urls), [["stun:stun.cloudflare.com:3478"]])

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertEqual(request.path, "/v1/ice-servers")
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
        XCTAssertTrue(request.body.isEmpty)
    }

    func testTurnCredentialsSendThePeerIdAndAcceptStringOrArrayUrls() async throws {
        StubProtocol.stub(
            path: "/v1/turn-credentials",
            status: 201,
            body: """
            {
              "iceServers": [
                {"urls": "stun:stun.cloudflare.com:3478"},
                {
                  "urls": ["turn:turn.cloudflare.com:3478?transport=udp"],
                  "username": "turn-user-1",
                  "credential": "turn-password-1"
                }
              ],
              "expiresAt": 1700000120
            }
            """
        )
        let credentials = try await client().turnCredentials(for: record())
        XCTAssertEqual(credentials.iceServers[0].urls, ["stun:stun.cloudflare.com:3478"])
        XCTAssertEqual(credentials.iceServers[1].username, "turn-user-1")
        XCTAssertEqual(credentials.expiresAt, 1_700_000_120)

        let request = try XCTUnwrap(StubProtocol.requests.first)
        XCTAssertTrue(request.body.contains("\"peerDeviceId\":\"\(macId)\""))
        XCTAssertEqual(request.headers["Authorization"], "Bearer token-1")
    }

    func testTurnCredentialsDistinguishRelayDisabledFromARevokedPairing() async throws {
        StubProtocol.stub(
            path: "/v1/turn-credentials",
            status: 403,
            body: #"{"error":"forbidden","reason":"relay access is disabled for this account"}"#
        )
        do {
            _ = try await client().turnCredentials(peerDeviceId: macId, accessToken: token)
            XCTFail("expected relayDisabled")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .relayDisabled("relay access is disabled for this account"))
            XCTAssertFalse(error.isRetryable)
            XCTAssertFalse(error.message.contains("pairing code"))
        }

        StubProtocol.reset()
        StubProtocol.stub(
            path: "/v1/turn-credentials",
            status: 403,
            body: #"{"error":"forbidden","reason":"TURN credentials require an active pairing"}"#
        )
        do {
            _ = try await client().turnCredentials(peerDeviceId: macId, accessToken: token)
            XCTFail("expected rejected")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .rejected("TURN credentials require an active pairing"))
            XCTAssertFalse(error.isRetryable)
        }
    }

    // MARK: - Local preflight

    func testCandidatesRefuseLoopbackHostnamesOverlongListsAndStaleExpiries() throws {
        XCTAssertThrowsError(
            try TransportCandidate.shortLived(address: "127.0.0.1:49221", now: now)
        )
        XCTAssertThrowsError(
            try TransportCandidate(
                address: "203.0.113.7:41234",
                expiresAt: 1_700_000_060,
                type: "host"
            ).validated(now: now, maxLifetime: 60)
        )
        XCTAssertThrowsError(
            try TransportCandidate(
                address: "203.0.113.7:41234",
                expiresAt: 1_700_000_060,
                type: "host",
                priority: 1,
                foundation: "host",
                component: 1,
                protocol: "udp",
                relatedAddress: "127.0.0.1",
                relatedPort: 4000
            ).validated(now: now, maxLifetime: 60)
        )
        XCTAssertThrowsError(
            try TransportCandidate(
                address: "203.0.113.7:41234",
                expiresAt: 1_700_000_060,
                type: "host",
                priority: 1,
                foundation: "host",
                component: 1,
                protocol: "udp",
                tcpType: "passive"
            ).validated(now: now, maxLifetime: 60)
        )
        XCTAssertThrowsError(
            try TransportCandidate.shortLived(address: "[::1]:49221", now: now)
        )
        XCTAssertThrowsError(
            try TransportCandidate.shortLived(address: "mac.example:49221", now: now)
        )
        XCTAssertThrowsError(
            try TransportCandidate.shortLived(address: "192.168.1.20:0", now: now)
        )

        let valid = try candidate()
        XCTAssertThrowsError(
            try TransportCandidate.prepare(
                Array(repeating: valid, count: 9),
                now: now,
                maxLifetime: 60
            )
        )
        XCTAssertThrowsError(
            try TransportCandidate.prepare([], now: now, maxLifetime: 60)
        )
        XCTAssertThrowsError(
            try TransportCandidate(
                address: "203.0.113.7:41234",
                expiresAt: 1_700_000_000
            ).validated(now: now, maxLifetime: 60)
        )
    }

    func testICECredentialsMustBeValidPairsBeforeTheNetworkCall() async throws {
        do {
            _ = try await client().publishPresence(
                candidates: [try candidate()],
                iceUfrag: "ufrag",
                icePwd: nil,
                accessToken: token
            )
            XCTFail("expected invalidCandidate")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .invalidCandidate("ICE ufrag and password must be supplied together."))
        }
        XCTAssertTrue(StubProtocol.requests.isEmpty)
    }

    func testPublishPresenceDoesNotHitTheNetworkWithALoopbackCandidate() async throws {
        do {
            _ = try await client().publishPresence(
                candidates: [
                    TransportCandidate(address: "127.0.0.1:9", expiresAt: 1_700_000_060)
                ],
                accessToken: token
            )
            XCTFail("expected invalidCandidate")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(
                error,
                .invalidCandidate(
                    "127.0.0.1:9 is a loopback address; the control plane must never see one."
                )
            )
        }
        XCTAssertTrue(StubProtocol.requests.isEmpty)
    }

    func testAPairingWithoutAMacDeviceIdIsTheManualLinkPath() async throws {
        do {
            _ = try await client().macPresence(for: record(macDeviceId: nil))
            XCTFail("expected manualLinkOnly")
        } catch let error as ControlPlaneError {
            XCTAssertEqual(error, .manualLinkOnly)
            XCTAssertFalse(error.isRetryable)
            XCTAssertTrue(error.message.contains("typed latch serve"))
        }
        XCTAssertTrue(StubProtocol.requests.isEmpty)
    }

    func testAlreadyPairedStillMapsTheWayPairingScreensExpect() {
        let error = HTTPControlPlaneClient.error(
            status: 409,
            path: "/v1/pairings/abc/confirm",
            data: Data(#"{"error":"already_paired"}"#.utf8)
        )
        XCTAssertEqual(error, .alreadyPaired)
        XCTAssertFalse(error.isRetryable)
    }
}
