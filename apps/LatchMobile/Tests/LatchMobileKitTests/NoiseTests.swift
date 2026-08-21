import CryptoKit
import XCTest
@testable import LatchMobileKit

/// The interoperability evidence for the phone's Noise implementation.
///
/// The transcript vectors below are not hand-derived: they were produced by
/// running the Mac's own stack — `snow` at the version in `Cargo.lock`, with
/// `NOISE_PATTERN` and no prologue, exactly as `responder_handshake` builds
/// it — against fixed static and ephemeral keys, and printing every message.
/// Matching them byte for byte is the strongest statement available without
/// a Mac on the network: this implementation and the Mac's produce the same
/// wire bytes, the same transcript hash, and the same transport keys.
final class NoiseTests: XCTestCase {

    // MARK: - Fixtures

    private static let initiatorStaticHex =
        "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
    private static let responderStaticHex =
        "a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0"
    private static let initiatorEphemeralHex =
        "2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40"
    private static let responderEphemeralHex =
        "c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0"

    /// The responder's public static key for the fixture above, as `snow`
    /// derives it. This is the value a paired record would pin.
    private static let responderPublicHex =
        "ad438bfae31f6c093d61d4339255ea798092c9fadd07b97827f4b0ae9dee7c1c"
    private static let initiatorPublicHex =
        "07a37cbc142093c8b755dc1b10e86cb426374ad16aa853ed0bdfc0b2b86d1c7c"

    private static let message1Hex =
        "5869aff450549732cbaaed5e5df9b30a6da31cb0e5742bad5ad4a1a768f1a67b6f6e65"
    private static let message2Hex =
        "3a553d74792d727efa9b9a4cde3da1ad93f1a2d0c09cb639b1a3c0fda14cbe24"
        + "577efd6b55baacb5e46cab91d519646b56fe38b69342cf92befdb9ac12b92b43"
        + "ccbfd34464ac04962ee945a4f0e6978c6db289b58c79717453541ae4dcc73620"
        + "17b456"
    private static let message3Hex =
        "9f193b7f66de766f5eb04bac373db673f99cf593fcd77f929753858400e5bd93"
        + "600d86a057cc0a58aef000cf9e1359e57336c6d621c148c6e468840f5771069f"
        + "3ee069a6a0"
    private static let handshakeHashHex =
        "3a13d911a4b5693961c8c2071e28b2353dab55c91eb00315c5b32fc4af4d4d17"
    /// First transport record initiator to responder, plaintext
    /// `GET /v2/capabilities`.
    private static let initiatorRecordHex =
        "1ca64c904a22bef89959dcaa9bcfc54d6b048540002162ef066751076e32ca18ce8655c0"
    /// First transport record responder to initiator, plaintext
    /// `HTTP/1.1 200 OK`.
    private static let responderRecordHex =
        "0ac5d8b956d04910059d4681f8c50e9eaa7aac4a7bafd73ecc083cfe09d26f"

    private func key(_ hex: String) throws -> Curve25519.KeyAgreement.PrivateKey {
        let raw = try XCTUnwrap(HexCoding.decode(hex))
        return try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
    }

    private func data(_ hex: String) throws -> Data {
        try XCTUnwrap(HexCoding.decode(hex))
    }

    // MARK: - BLAKE2s

    func testBlake2sMatchesPublishedVectors() {
        XCTAssertEqual(
            HexCoding.encode(Blake2s.hash(Data())),
            "69217a3079908094e11121d042354a7c1f55b6482ca1a51e1b250dfd1ed0eef9"
        )
        XCTAssertEqual(
            HexCoding.encode(Blake2s.hash(Data("abc".utf8))),
            "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982"
        )
    }

    /// Anything over one 64-byte block exercises the streaming path and the
    /// final-block flag, which is where a hand-written BLAKE2 usually breaks.
    func testBlake2sSpansMultipleBlocks() {
        let long = Data(repeating: 0x61, count: 200)
        var streamed = Blake2s()
        for chunk in stride(from: 0, to: long.count, by: 7) {
            streamed.update(long[chunk..<min(chunk + 7, long.count)])
        }
        XCTAssertEqual(streamed.finalize(), Blake2s.hash(long))
        XCTAssertEqual(Blake2s.hash(long).count, 32)
    }

    // MARK: - Transcript against the Mac's implementation

    func testInitiatorProducesTheMacsHandshakeBytes() throws {
        let initiator = NoiseHandshake(
            role: .initiator,
            staticKey: try key(Self.initiatorStaticHex)
        )
        initiator.setEphemeralKeyForTesting(try key(Self.initiatorEphemeralHex))

        let first = try initiator.writeMessage(payload: Data("one".utf8))
        XCTAssertEqual(HexCoding.encode(first), Self.message1Hex)

        let payload = try initiator.readMessage(try data(Self.message2Hex))
        XCTAssertEqual(payload, Data("two".utf8))
        XCTAssertEqual(initiator.remoteStaticKey, Self.responderPublicHex)

        let third = try initiator.writeMessage(payload: Data("three".utf8))
        XCTAssertEqual(HexCoding.encode(third), Self.message3Hex)

        let session = try initiator.split()
        XCTAssertEqual(HexCoding.encode(session.handshakeHash), Self.handshakeHashHex)
        XCTAssertEqual(session.peerStaticKey, Self.responderPublicHex)
    }

    func testResponderProducesTheMacsHandshakeBytes() throws {
        let responder = NoiseHandshake(
            role: .responder,
            staticKey: try key(Self.responderStaticHex)
        )
        responder.setEphemeralKeyForTesting(try key(Self.responderEphemeralHex))

        XCTAssertEqual(try responder.readMessage(try data(Self.message1Hex)), Data("one".utf8))
        XCTAssertEqual(
            HexCoding.encode(try responder.writeMessage(payload: Data("two".utf8))),
            Self.message2Hex
        )
        XCTAssertEqual(try responder.readMessage(try data(Self.message3Hex)), Data("three".utf8))

        let session = try responder.split()
        XCTAssertEqual(HexCoding.encode(session.handshakeHash), Self.handshakeHashHex)
        XCTAssertEqual(session.peerStaticKey, Self.initiatorPublicHex)
    }

    /// The transport keys are the point of the handshake, so they are checked
    /// against real records rather than by a round trip against ourselves,
    /// which would pass even if both sides derived the same wrong key.
    func testTransportRecordsMatchTheMacsCiphertext() throws {
        let initiator = NoiseHandshake(
            role: .initiator,
            staticKey: try key(Self.initiatorStaticHex)
        )
        initiator.setEphemeralKeyForTesting(try key(Self.initiatorEphemeralHex))
        _ = try initiator.writeMessage(payload: Data("one".utf8))
        _ = try initiator.readMessage(try data(Self.message2Hex))
        _ = try initiator.writeMessage(payload: Data("three".utf8))
        let session = try initiator.split()

        XCTAssertEqual(
            HexCoding.encode(try session.encrypt(Data("GET /v2/capabilities".utf8))),
            Self.initiatorRecordHex
        )
        XCTAssertEqual(
            try session.decrypt(try data(Self.responderRecordHex)),
            Data("HTTP/1.1 200 OK".utf8)
        )
    }

    // MARK: - Peer verification

    func testConnectAcceptsThePinnedMac() async throws {
        let responderKey = try key(Self.responderStaticHex)
        let channel = StubResponderChannel(staticKey: responderKey)
        let session = try await NoiseXX.connect(
            channel: channel,
            staticKey: try key(Self.initiatorStaticHex),
            pinnedPeerPublicKey: Self.responderPublicHex
        )
        XCTAssertEqual(session.peerStaticKey, Self.responderPublicHex)
        XCTAssertEqual(session.handshakeHash, channel.responderHandshakeHash)
    }

    /// The impersonation case. The handshake itself succeeds — a machine in
    /// the middle can complete XX perfectly well with its own key — and it is
    /// the comparison against the paired record that stops it.
    func testConnectRejectsAMacThatIsNotThePinnedOne() async throws {
        let impostor = Curve25519.KeyAgreement.PrivateKey()
        let channel = StubResponderChannel(staticKey: impostor)
        do {
            _ = try await NoiseXX.connect(
                channel: channel,
                staticKey: try key(Self.initiatorStaticHex),
                pinnedPeerPublicKey: Self.responderPublicHex
            )
            XCTFail("a handshake with an unpinned static key must not produce a session")
        } catch let error as NoiseError {
            guard case .peerIdentityMismatch(let expected, let received) = error else {
                return XCTFail("expected an identity mismatch, got \(error)")
            }
            XCTAssertEqual(expected, Self.responderPublicHex)
            XCTAssertEqual(received, HexCoding.encode(impostor.publicKey.rawRepresentation))
            // Terminal, exactly like PairingModel.confirm(): retrying cannot
            // turn a different machine into the paired one.
            XCTAssertFalse(error.isRetryable)
            XCTAssertTrue(error.message.contains("refused"))
        }
    }

    /// The pin is the phone's own record, not anything the transport or the
    /// control plane asserted. Case is normalized because a record written by
    /// an older build may hold uppercase hex.
    func testPinComesFromThePairedRecordAndIsCaseInsensitive() async throws {
        let record = PairedDeviceRecord(
            deviceId: "dev_1",
            name: "iPhone",
            devicePublicKey: Self.initiatorPublicHex,
            mac: PairedMac(deviceId: "dev_mac", publicKey: Self.responderPublicHex.uppercased()),
            permission: .interact,
            phrase: "amber-otter-cliff"
        )
        XCTAssertEqual(try record.pinnedMacPublicKey(), Self.responderPublicHex)

        let channel = StubResponderChannel(staticKey: try key(Self.responderStaticHex))
        let session = try await NoiseXX.connect(
            channel: channel,
            staticKey: try key(Self.initiatorStaticHex),
            pinnedPeerPublicKey: try record.pinnedMacPublicKey()
        )
        XCTAssertEqual(session.peerStaticKey, Self.responderPublicHex)
    }

    func testAnUnusablePinIsRefusedBeforeAnyBytesAreSent() async throws {
        let channel = StubResponderChannel(staticKey: try key(Self.responderStaticHex))
        do {
            _ = try await NoiseXX.connect(
                channel: channel,
                staticKey: try key(Self.initiatorStaticHex),
                pinnedPeerPublicKey: "not-a-key"
            )
            XCTFail("an unusable pin must not start a handshake")
        } catch let error as NoiseError {
            guard case .unusablePin = error else {
                return XCTFail("expected an unusable pin, got \(error)")
            }
            XCTAssertEqual(channel.framesWritten, 0)
        }
    }

    /// A prologue mismatch is the quiet failure mode: the Mac's LAN responder
    /// uses none, so a phone that invented one would fail to decrypt the
    /// second message rather than getting a clear answer.
    func testAPrologueMismatchFailsTheHandshake() async throws {
        let channel = StubResponderChannel(staticKey: try key(Self.responderStaticHex))
        do {
            _ = try await NoiseXX.connect(
                channel: channel,
                staticKey: try key(Self.initiatorStaticHex),
                pinnedPeerPublicKey: Self.responderPublicHex,
                prologue: Data("latch-relay-v1:abc".utf8)
            )
            XCTFail("a prologue the Mac did not use must not produce a session")
        } catch let error as NoiseError {
            XCTAssertEqual(error, .decryptionFailed)
        }
    }

    // MARK: - Framing

    func testFramingMatchesTheMacsLengthPrefix() throws {
        let frame = Data([0xde, 0xad, 0xbe, 0xef])
        let encoded = try NoiseFraming.encode(frame)
        XCTAssertEqual(Array(encoded.prefix(2)), [0x00, 0x04])

        let decoded = try XCTUnwrap(try NoiseFraming.decode(from: encoded + Data([0x01])))
        XCTAssertEqual(decoded.frame, frame)
        XCTAssertEqual(decoded.rest, Data([0x01]))

        // A partial frame is not an error; it is more bytes to wait for.
        XCTAssertNil(try NoiseFraming.decode(from: encoded.prefix(3)))
        // Both ends reject an empty frame and a zero length outright.
        XCTAssertThrowsError(try NoiseFraming.encode(Data()))
        XCTAssertThrowsError(try NoiseFraming.decode(from: Data([0x00, 0x00])))
    }

    func testRecordsOverTheMacsLimitAreRefusedLocally() async throws {
        let channel = StubResponderChannel(staticKey: try key(Self.responderStaticHex))
        let session = try await NoiseXX.connect(
            channel: channel,
            staticKey: try key(Self.initiatorStaticHex),
            pinnedPeerPublicKey: Self.responderPublicHex
        )
        XCTAssertThrowsError(
            try session.encrypt(Data(repeating: 0x41, count: NoiseWire.maxPlaintext + 1))
        )
        XCTAssertNoThrow(
            try session.encrypt(Data(repeating: 0x41, count: NoiseWire.maxPlaintext))
        )
    }
}

/// An in-process XX responder standing in for the Mac, so the whole handshake
/// runs under plain `swift test` with no simulator and no network. It drives
/// the same `NoiseHandshake` in `.responder` role that the transcript vectors
/// above proved byte-identical to the Mac's `responder_handshake`.
private final class StubResponderChannel: NoiseFrameChannel, @unchecked Sendable {
    private let lock = NSLock()
    private let handshake: NoiseHandshake
    private var pending: [Data] = []
    private(set) var framesWritten = 0
    private(set) var responderHandshakeHash: Data?

    init(staticKey: Curve25519.KeyAgreement.PrivateKey) {
        handshake = NoiseHandshake(role: .responder, staticKey: staticKey)
    }

    func readFrame() async throws -> Data {
        lock.lock()
        defer { lock.unlock() }
        guard !pending.isEmpty else {
            throw NoiseError.transport("the stub responder has nothing to send")
        }
        return pending.removeFirst()
    }

    func writeFrame(_ frame: Data) async throws {
        lock.lock()
        defer { lock.unlock() }
        framesWritten += 1
        _ = try handshake.readMessage(frame)
        if handshake.isWriteTurn {
            pending.append(try handshake.writeMessage())
        }
        if handshake.isComplete {
            responderHandshakeHash = try handshake.split().handshakeHash
        }
    }
}
