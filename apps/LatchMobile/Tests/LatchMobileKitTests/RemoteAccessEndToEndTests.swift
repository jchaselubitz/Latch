import CryptoKit
import XCTest

@testable import LatchMobileKit

/// A composed phone-side regression for the entire paired route. The network
/// seams are deterministic, but every security boundary is real: pairing pins
/// the Mac key, signaling is bearer-authenticated, and Noise proves that pin
/// before the existing gateway client lists or sends anything.
@MainActor
final class RemoteAccessEndToEndTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let controlPlane = URL(string: "https://control.example")!

    override func setUp() {
        super.setUp()
        StubProtocol.reset()
    }

    func testPairedPhoneFlowSignalsPinsNoiseUsesGatewayAndStopsOnRevocation() async throws {
        let macPrivateKey = Curve25519.KeyAgreement.PrivateKey()
        let macPublicKey = HexCoding.encode(macPrivateKey.publicKey.rawRepresentation)
        let pairingControl = EndToEndControlPlane(
            confirmation: PairingConfirmation(
                device: .init(
                    deviceId: "dev_phone",
                    name: "Test iPhone",
                    permission: .interact,
                    revoked: false
                ),
                mac: .init(deviceId: "dev_mac", publicKey: macPublicKey, name: "Studio Mac"),
                accessToken: "phone-access-token"
            )
        )
        let identities = MemoryDeviceIdentityStore()
        let pairings = MemoryPairedDeviceStore()
        let pairing = PairingModel(
            identityStore: identities,
            deviceStore: pairings,
            camera: StubCameraAuthorization(.authorized),
            deviceName: "Test iPhone",
            clientFactory: { _ in pairingControl }
        )

        await pairing.restore()
        pairing.scanned(pairingCode(for: macPublicKey), now: now)
        await pairing.confirm(now: now)
        let record = try XCTUnwrap(pairing.record)
        XCTAssertEqual(record.mac.publicKey, macPublicKey)
        XCTAssertEqual(record.accessToken, "phone-access-token")

        stubSignalingAndGateway(macPublicKey: macPublicKey)
        let signaling = HTTPControlPlaneClient(baseURL: controlPlane, session: StubProtocol.session())
        let candidate = try TransportCandidate.shortLived(
            address: "203.0.113.7:40123",
            lifetime: SignalingWindows.rendezvousTTL
        )
        _ = try await signaling.publishPresence(
            candidates: [candidate],
            accessToken: try record.signalingAccessToken()
        )
        let presence = try await signaling.macPresence(for: record)
        XCTAssertTrue(presence.online)
        let rendezvous = try await signaling.offerRendezvous(
            for: record,
            candidates: [candidate],
            requestId: "request-phone-mac"
        )
        XCTAssertNotEqual(
            rendezvous.peerIdentityKey,
            record.mac.publicKey,
            "the control-plane claim must not become the Noise pin"
        )

        let noise = try await NoiseXX.connect(
            channel: EndToEndNoiseResponder(staticKey: macPrivateKey),
            staticKey: try identities.privateKey(),
            pinnedPeerPublicKey: try record.pinnedMacPublicKey()
        )
        XCTAssertEqual(noise.peerStaticKey, macPublicKey)

        let model = AppModel(
            storage: MemoryLinkStorage(),
            sessionFactory: { link in LatchGateway(link: link, session: StubProtocol.session()) },
            pairedGatewayFactory: { paired in
                XCTAssertEqual(paired.mac.publicKey, macPublicKey)
                let link = try GatewayLink(address: "http://127.0.0.1:8787", token: "")
                return LatchGateway(link: link, session: StubProtocol.session())
            }
        )
        await model.connectPairedDevice(record)
        XCTAssertEqual(model.linkSource, .paired)
        XCTAssertEqual(model.sessions.map(\.id), ["ses_1"])

        model.suspendConversations()
        model.suspendPairedTransport()
        await model.resumeAfterSuspension()
        XCTAssertEqual(model.linkSource, .paired)
        XCTAssertEqual(model.sessions.map(\.id), ["ses_1"])

        pairingControl.markRevoked()
        await pairing.refreshPermission()
        XCTAssertTrue(pairing.record?.revoked == true)
        await model.connectPairedDevice(pairing.record)
        XCTAssertEqual(model.linkState, .unlinked)
        XCTAssertNil(model.gateway)

        let requests = StubProtocol.requests
        XCTAssertTrue(requests.contains { $0.path == "/v1/presence" && $0.headers["Authorization"] == "Bearer phone-access-token" })
        XCTAssertTrue(requests.contains { $0.path == "/v1/presence/dev_mac" && $0.headers["Authorization"] == "Bearer phone-access-token" })
        XCTAssertTrue(requests.contains { $0.path == "/v1/rendezvous" && $0.headers["Authorization"] == "Bearer phone-access-token" })
        XCTAssertFalse(
            requests.contains { request in
                ["/v2/capabilities", "/v2/sessions"].contains(request.path)
                    && request.headers["Authorization"] != nil
            },
            "the paired tunnel never sends the gateway credential from the phone"
        )
    }

    private func pairingCode(for macPublicKey: String) -> String {
        let object: [String: Any] = [
            "formatVersion": 1,
            "pairingId": "0123456789abcdef0123456789abcdef",
            "secret": String(repeating: "a", count: 64),
            "macPublicKey": macPublicKey,
            "expiresAt": now.addingTimeInterval(240).timeIntervalSince1970,
            "controlPlane": controlPlane.absoluteString,
            "macName": "Studio Mac",
        ]
        return String(data: try! JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }

    private func stubSignalingAndGateway(macPublicKey: String) {
        StubProtocol.stub(
            path: "/v1/presence",
            body: #"{"deviceId":"dev_phone","expiresAt":1700000090,"ttlSeconds":90}"#
        )
        StubProtocol.stub(
            path: "/v1/presence/dev_mac",
            body: """
            {"deviceId":"dev_mac","online":true,"identityKey":"\(macPublicKey)",
             "candidates":[{"address":"198.51.100.2:49221","expiresAt":1700000060}],
             "expiresAt":1700000060}
            """
        )
        StubProtocol.stub(
            path: "/v1/rendezvous",
            body: """
            {"requestId":"request-phone-mac","peerDeviceId":"dev_mac",
             "peerIdentityKey":"\(String(repeating: "e", count: 64))","permission":"interact",
             "candidates":[{"address":"198.51.100.2:49221","expiresAt":1700000060}],
             "expiresAt":1700000060}
            """
        )
        StubProtocol.stub(
            path: "/v2/capabilities",
            body: """
            {"protocolVersion":2,"productVersion":"2.0.0",
             "capabilities":{"create":true,"openViewer":true,"localAttach":true,
              "cloudAttach":false,"selfUpdate":true,"extensions":[]},
             "endpoints":{"sessions":true,"terminal":true,"conversation":false},
             "features":{"readOnlyTerminal":true},"gatewayInstanceId":"paired-gateway",
             "operationRetentionSeconds":600}
            """
        )
        StubProtocol.stub(
            path: "/v2/sessions",
            body: #"{"sessions":[{"id":"ses_1","name":"work","state":"running","cwd":"/work","command_label":"latch","created_at":"2026-08-16T12:00:00Z"}]}"#
        )
    }
}

private final class EndToEndControlPlane: ControlPlaneClient, @unchecked Sendable {
    private let lock = NSLock()
    private let confirmation: PairingConfirmation
    private var revoked = false

    init(confirmation: PairingConfirmation) {
        self.confirmation = confirmation
    }

    func enroll(pairingId _: String, enrollment _: PairingEnrollment) async throws -> PairingConfirmation {
        confirmation
    }

    func device(deviceId _: String, accessToken _: String) async throws -> PairingConfirmation {
        guard lock.withLock({ revoked }) else { return confirmation }
        return PairingConfirmation(
            device: .init(
                deviceId: confirmation.device.deviceId,
                name: confirmation.device.name,
                permission: confirmation.device.permission,
                revoked: true
            ),
            mac: confirmation.mac,
            accessToken: confirmation.accessToken
        )
    }

    func revoke(deviceId _: String, accessToken _: String) async throws {}

    func markRevoked() {
        lock.lock()
        revoked = true
        lock.unlock()
    }
}

private final class EndToEndNoiseResponder: NoiseFrameChannel, @unchecked Sendable {
    private let lock = NSLock()
    private let handshake: NoiseHandshake
    private var pending: [Data] = []

    init(staticKey: Curve25519.KeyAgreement.PrivateKey) {
        handshake = NoiseHandshake(role: .responder, staticKey: staticKey)
    }

    func readFrame() async throws -> Data {
        try lock.withLock {
            guard !pending.isEmpty else { throw NoiseError.transport("no pending responder frame") }
            return pending.removeFirst()
        }
    }

    func writeFrame(_ frame: Data) async throws {
        try lock.withLock {
            _ = try handshake.readMessage(frame)
            if handshake.isWriteTurn {
                pending.append(try handshake.writeMessage())
            }
        }
    }
}
