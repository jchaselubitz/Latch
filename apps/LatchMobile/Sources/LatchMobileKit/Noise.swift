import CryptoKit
import Foundation

/// The phone's half of `Noise_XX_25519_ChaChaPoly_BLAKE2s`.
///
/// This is the same pattern, suite, and framing the Mac's responder uses in
/// `crates/latch/src/cli/remote_access.rs` (`NOISE_PATTERN`,
/// `responder_handshake`, `read_frame`/`write_frame`). The Mac's LAN responder
/// is built with no prologue, so the default here is an empty one; anything
/// else would produce a decrypt failure on the second message rather than an
/// obvious error, which is why the prologue is an explicit parameter instead
/// of an implicit constant.
///
/// The static keypair on this side already exists: `DeviceIdentity` creates
/// and stores it, and this is the handshake its comment reserved the secret
/// for. Nothing here reads the keychain — the caller hands in the key, so the
/// handshake is testable with an ephemeral identity.

// MARK: - Errors

/// Why a Noise handshake or transport record failed.
///
/// Every case here is fatal to the connection. Only `transport` is worth
/// retrying, and that is a socket problem rather than a cryptographic one.
public enum NoiseError: Error, Equatable, Sendable {
    /// The peer's static key is not the key this phone paired with. This is
    /// the impersonation case: the pin came from the QR code the person read
    /// off their own Mac, so a different key means a different machine.
    case peerIdentityMismatch(expected: String, received: String)
    /// The pinned key on the paired record is not 32 bytes of hex, so there
    /// is nothing to verify against and the connection must not proceed.
    case unusablePin(String)
    /// A handshake message was the wrong length, or arrived out of order.
    case malformedHandshakeMessage(String)
    /// A ciphertext failed authentication.
    case decryptionFailed
    /// The peer completed the handshake without presenting a static key.
    case missingPeerIdentity
    /// A record was empty or larger than the Mac's frame limit.
    case recordTooLarge(Int)
    /// The nonce space for this session is exhausted; reconnect.
    case nonceExhausted
    /// Key agreement itself failed, which for X25519 means a degenerate key.
    case keyAgreementFailed(String)
    /// The socket underneath failed.
    case transport(String)

    public var message: String {
        switch self {
        case .peerIdentityMismatch(let expected, let received):
            return """
            This Mac answered with identity \(HexCoding.abbreviate(received)), but this phone \
            is paired with \(HexCoding.abbreviate(expected)). The connection was refused.
            """
        case .unusablePin(let detail):
            return "This pairing's stored Mac identity is unusable: \(detail)"
        case .malformedHandshakeMessage(let detail):
            return "Your Mac sent a handshake this phone could not read: \(detail)"
        case .decryptionFailed:
            return "A message from your Mac failed its authentication check."
        case .missingPeerIdentity:
            return "Your Mac finished the handshake without proving which Mac it is."
        case .recordTooLarge(let size):
            return "A \(size)-byte message is outside what the remote-access transport carries."
        case .nonceExhausted:
            return "This connection has been open too long and must be re-established."
        case .keyAgreementFailed(let detail):
            return "Key agreement with your Mac failed: \(detail)"
        case .transport(let detail):
            return detail
        }
    }

    /// A mismatch is never retried. Repeating a handshake against a machine
    /// that is not the paired Mac cannot start succeeding, and retrying would
    /// turn a stated impersonation failure into a spinner. This mirrors
    /// `PairingModel.confirm()`, which stops the pairing outright rather than
    /// offering another attempt.
    public var isRetryable: Bool {
        switch self {
        case .transport: return true
        default: return false
        }
    }
}

// MARK: - Wire limits

/// Limits the Mac imposes on the framed transport, mirrored here so the phone
/// fails locally with a sentence instead of having a frame dropped.
public enum NoiseWire {
    /// `MAX_RECORD` in `remote_access.rs`: the u16 length prefix's ceiling.
    public static let maxRecord = 65_535
    /// The Mac refuses to encrypt a plaintext larger than this, leaving room
    /// for the ChaChaPoly tag inside one record.
    public static let maxPlaintext = 65_535 - 32
}

// MARK: - Cipher state

/// One direction of an established session: a key and a monotonic counter.
///
/// Noise's ChaChaPoly nonce is 32 zero bits followed by the 64-bit counter in
/// little-endian order, which is not the same as the IETF construction's
/// big-endian-adjacent layout, so it is built by hand here.
struct NoiseCipherState {
    private var key: SymmetricKey?
    private var nonce: UInt64 = 0

    init(key: SymmetricKey? = nil) {
        self.key = key
    }

    var hasKey: Bool { key != nil }

    mutating func initializeKey(_ material: Data) {
        key = SymmetricKey(data: material)
        nonce = 0
    }

    /// Encrypts with associated data. With no key this is the identity
    /// function, which is what Noise's `EncryptWithAd` does before the first
    /// DH and is why the first handshake message is plaintext.
    mutating func encrypt(associatedData: Data, plaintext: Data) throws -> Data {
        guard let key else { return plaintext }
        guard nonce != UInt64.max else { throw NoiseError.nonceExhausted }
        let sealed = try ChaChaPoly.seal(
            plaintext,
            using: key,
            nonce: try Self.chaChaNonce(nonce),
            authenticating: associatedData
        )
        nonce &+= 1
        return sealed.ciphertext + sealed.tag
    }

    mutating func decrypt(associatedData: Data, ciphertext: Data) throws -> Data {
        guard let key else { return ciphertext }
        guard nonce != UInt64.max else { throw NoiseError.nonceExhausted }
        guard ciphertext.count >= 16 else {
            throw NoiseError.malformedHandshakeMessage("a ciphertext was shorter than its tag")
        }
        let split = ciphertext.count - 16
        let body = ciphertext.prefix(split)
        let tag = ciphertext.suffix(16)
        do {
            let box = try ChaChaPoly.SealedBox(
                nonce: try Self.chaChaNonce(nonce),
                ciphertext: body,
                tag: tag
            )
            let plaintext = try ChaChaPoly.open(box, using: key, authenticating: associatedData)
            nonce &+= 1
            return plaintext
        } catch {
            throw NoiseError.decryptionFailed
        }
    }

    private static func chaChaNonce(_ counter: UInt64) throws -> ChaChaPoly.Nonce {
        var bytes = Data(repeating: 0, count: 4)
        withUnsafeBytes(of: counter.littleEndian) { bytes.append(contentsOf: $0) }
        return try ChaChaPoly.Nonce(data: bytes)
    }
}

// MARK: - Symmetric state

/// The running transcript: chaining key plus handshake hash.
///
/// Every handshake byte, in the order it went on the wire, is mixed into `h`,
/// so the two ends only agree if they saw the same messages. That is what
/// makes the transcript tests meaningful — two independent implementations
/// landing on the same `h` is evidence they built the same transcript.
struct NoiseSymmetricState {
    private(set) var chainingKey: Data
    private(set) var handshakeHash: Data
    var cipher = NoiseCipherState()

    init(protocolName: String) {
        let name = Data(protocolName.utf8)
        // Names at or under HASHLEN are used verbatim and zero-padded; longer
        // ones are hashed. This suite's name is 33 bytes, so it is hashed.
        if name.count <= Blake2s.digestLength {
            handshakeHash = name + Data(repeating: 0, count: Blake2s.digestLength - name.count)
        } else {
            handshakeHash = NoiseHash.hash(name)
        }
        chainingKey = handshakeHash
    }

    mutating func mixKey(_ material: Data) {
        let outputs = NoiseHash.hkdf(chainingKey: chainingKey, material: material, outputs: 2)
        chainingKey = outputs[0]
        cipher.initializeKey(outputs[1])
    }

    mutating func mixHash(_ data: Data) {
        handshakeHash = NoiseHash.hash(handshakeHash + data)
    }

    mutating func encryptAndHash(_ plaintext: Data) throws -> Data {
        let ciphertext = try cipher.encrypt(associatedData: handshakeHash, plaintext: plaintext)
        mixHash(ciphertext)
        return ciphertext
    }

    mutating func decryptAndHash(_ ciphertext: Data) throws -> Data {
        let plaintext = try cipher.decrypt(associatedData: handshakeHash, ciphertext: ciphertext)
        mixHash(ciphertext)
        return plaintext
    }

    /// Derives the two transport keys. The first is the initiator's send key.
    func split() -> (Data, Data) {
        let outputs = NoiseHash.hkdf(chainingKey: chainingKey, material: Data(), outputs: 2)
        return (outputs[0], outputs[1])
    }
}

// MARK: - Handshake

/// An established session: one cipher per direction, plus the peer's proven
/// static key and the final handshake hash.
public final class NoiseSession: @unchecked Sendable {
    private let lock = NSLock()
    private var sending: NoiseCipherState
    private var receiving: NoiseCipherState

    /// The peer's static public key as lowercase hex, proven by the handshake
    /// rather than asserted by anyone. For a phone-initiated connection this
    /// has already been checked against the paired record.
    public let peerStaticKey: String
    /// The final handshake hash, suitable for channel binding and for the
    /// transcript tests that compare this implementation against the Mac's.
    public let handshakeHash: Data

    init(sending: NoiseCipherState, receiving: NoiseCipherState, peerStaticKey: String, handshakeHash: Data) {
        self.sending = sending
        self.receiving = receiving
        self.peerStaticKey = peerStaticKey
        self.handshakeHash = handshakeHash
    }

    /// Encrypts one transport record. The size bound matches the Mac's
    /// `encrypt_transport_record`, so a record this accepts is one the Mac
    /// will accept too.
    public func encrypt(_ plaintext: Data) throws -> Data {
        guard !plaintext.isEmpty, plaintext.count <= NoiseWire.maxPlaintext else {
            throw NoiseError.recordTooLarge(plaintext.count)
        }
        lock.lock()
        defer { lock.unlock() }
        return try sending.encrypt(associatedData: Data(), plaintext: plaintext)
    }

    public func decrypt(_ ciphertext: Data) throws -> Data {
        guard !ciphertext.isEmpty, ciphertext.count <= NoiseWire.maxRecord else {
            throw NoiseError.recordTooLarge(ciphertext.count)
        }
        lock.lock()
        defer { lock.unlock() }
        return try receiving.decrypt(associatedData: Data(), ciphertext: ciphertext)
    }
}

/// The XX handshake in either role.
///
/// The responder role exists so the test suite can run a complete handshake
/// in process without a Mac or a simulator. The app only ever uses
/// `.initiator`.
public final class NoiseHandshake {
    public enum Role: Sendable {
        case initiator
        case responder
    }

    /// One token of the XX pattern.
    private enum Token {
        case e, s, ee, es, se
    }

    /// `Noise_XX_25519_ChaChaPoly_BLAKE2s`: -> e / <- e, ee, s, es / -> s, se.
    private static let pattern: [[Token]] = [
        [.e],
        [.e, .ee, .s, .es],
        [.s, .se]
    ]

    public static let protocolName = "Noise_XX_25519_ChaChaPoly_BLAKE2s"

    private let role: Role
    private var symmetric: NoiseSymmetricState
    private let staticKey: Curve25519.KeyAgreement.PrivateKey
    private var ephemeralKey: Curve25519.KeyAgreement.PrivateKey?
    private var remoteEphemeral: Curve25519.KeyAgreement.PublicKey?
    private var remoteStatic: Curve25519.KeyAgreement.PublicKey?
    private var messageIndex = 0

    public init(
        role: Role,
        staticKey: Curve25519.KeyAgreement.PrivateKey,
        prologue: Data = Data()
    ) {
        self.role = role
        self.staticKey = staticKey
        var symmetric = NoiseSymmetricState(protocolName: Self.protocolName)
        symmetric.mixHash(prologue)
        self.symmetric = symmetric
    }

    /// Test seam: fixes the ephemeral key so a transcript is reproducible.
    /// Production never calls this — an ephemeral that is not fresh is not
    /// ephemeral.
    func setEphemeralKeyForTesting(_ key: Curve25519.KeyAgreement.PrivateKey) {
        ephemeralKey = key
    }

    /// Whether all three messages have been processed.
    public var isComplete: Bool { messageIndex >= Self.pattern.count }

    /// The peer's static key as lowercase hex, once it has been received.
    public var remoteStaticKey: String? {
        remoteStatic.map { HexCoding.encode($0.rawRepresentation) }
    }

    /// The transcript hash so far.
    public var handshakeHash: Data { symmetric.handshakeHash }

    /// Whether it is this side's turn to write.
    public var isWriteTurn: Bool {
        guard !isComplete else { return false }
        let initiatorWrites = messageIndex % 2 == 0
        return (role == .initiator) == initiatorWrites
    }

    public func writeMessage(payload: Data = Data()) throws -> Data {
        guard isWriteTurn else {
            throw NoiseError.malformedHandshakeMessage("it is not this side's turn to write")
        }
        var buffer = Data()
        for token in Self.pattern[messageIndex] {
            switch token {
            case .e:
                let key = ephemeralKey ?? Curve25519.KeyAgreement.PrivateKey()
                ephemeralKey = key
                let publicKey = key.publicKey.rawRepresentation
                buffer.append(publicKey)
                symmetric.mixHash(publicKey)
            case .s:
                let encrypted = try symmetric.encryptAndHash(staticKey.publicKey.rawRepresentation)
                buffer.append(encrypted)
            case .ee, .es, .se:
                symmetric.mixKey(try agreement(for: token))
            }
        }
        buffer.append(try symmetric.encryptAndHash(payload))
        messageIndex += 1
        return buffer
    }

    /// Consumes one handshake message and returns its payload.
    @discardableResult
    public func readMessage(_ message: Data) throws -> Data {
        guard !isComplete, !isWriteTurn else {
            throw NoiseError.malformedHandshakeMessage("it is not this side's turn to read")
        }
        var rest = Data(message)
        for token in Self.pattern[messageIndex] {
            switch token {
            case .e:
                let (field, remainder) = try Self.take(32, from: rest, what: "an ephemeral key")
                remoteEphemeral = try Self.publicKey(field)
                symmetric.mixHash(field)
                rest = remainder
            case .s:
                // 32 bytes, plus a tag once the transcript has a key. In XX the
                // static is always sent after a DH, so the tag is always there,
                // but deriving the length keeps this honest to the state.
                let length = symmetric.cipher.hasKey ? 48 : 32
                let (field, remainder) = try Self.take(length, from: rest, what: "a static key")
                let decrypted = try symmetric.decryptAndHash(field)
                remoteStatic = try Self.publicKey(decrypted)
                rest = remainder
            case .ee, .es, .se:
                symmetric.mixKey(try agreement(for: token))
            }
        }
        let payload = try symmetric.decryptAndHash(rest)
        messageIndex += 1
        return payload
    }

    /// Splits into transport ciphers. Fails if the peer never presented a
    /// static key, which for XX means the handshake did not actually finish.
    public func split() throws -> NoiseSession {
        guard isComplete else {
            throw NoiseError.malformedHandshakeMessage("the handshake is not finished")
        }
        guard let remoteStaticKey else { throw NoiseError.missingPeerIdentity }
        let (first, second) = symmetric.split()
        // The first output is the initiator's sending key on both ends; the
        // responder simply uses them the other way around.
        let sending = role == .initiator ? first : second
        let receiving = role == .initiator ? second : first
        return NoiseSession(
            sending: NoiseCipherState(key: SymmetricKey(data: sending)),
            receiving: NoiseCipherState(key: SymmetricKey(data: receiving)),
            peerStaticKey: remoteStaticKey,
            handshakeHash: symmetric.handshakeHash
        )
    }

    private func agreement(for token: Token) throws -> Data {
        let local: Curve25519.KeyAgreement.PrivateKey
        let remote: Curve25519.KeyAgreement.PublicKey?
        switch token {
        case .ee:
            local = try requireEphemeral()
            remote = remoteEphemeral
        case .es:
            // es is the initiator's ephemeral against the responder's static.
            if role == .initiator {
                local = try requireEphemeral()
                remote = remoteStatic
            } else {
                local = staticKey
                remote = remoteEphemeral
            }
        case .se:
            // se is the initiator's static against the responder's ephemeral.
            if role == .initiator {
                local = staticKey
                remote = remoteEphemeral
            } else {
                local = try requireEphemeral()
                remote = remoteStatic
            }
        case .e, .s:
            throw NoiseError.malformedHandshakeMessage("\(token) is not a key-agreement token")
        }
        guard let remote else {
            throw NoiseError.malformedHandshakeMessage("a key agreement ran before its key arrived")
        }
        do {
            let shared = try local.sharedSecretFromKeyAgreement(with: remote)
            return shared.withUnsafeBytes { Data($0) }
        } catch {
            throw NoiseError.keyAgreementFailed(String(describing: error))
        }
    }

    private func requireEphemeral() throws -> Curve25519.KeyAgreement.PrivateKey {
        guard let ephemeralKey else {
            throw NoiseError.malformedHandshakeMessage("a key agreement ran before the ephemeral key")
        }
        return ephemeralKey
    }

    private static func take(
        _ count: Int,
        from data: Data,
        what: String
    ) throws -> (Data, Data) {
        guard data.count >= count else {
            throw NoiseError.malformedHandshakeMessage("the message was too short for \(what)")
        }
        return (data.prefix(count), data.dropFirst(count))
    }

    private static func publicKey(_ data: Data) throws -> Curve25519.KeyAgreement.PublicKey {
        do {
            return try Curve25519.KeyAgreement.PublicKey(rawRepresentation: data)
        } catch {
            throw NoiseError.malformedHandshakeMessage("a public key was not a valid X25519 point")
        }
    }
}

// MARK: - Framing

/// The Mac's frame layout: a big-endian u16 length, then that many bytes.
///
/// `read_frame` in `remote_access.rs` rejects a zero length and anything over
/// `MAX_RECORD`, and `write_frame` refuses to emit either, so both rules are
/// enforced on this side too.
public enum NoiseFraming {
    public static func encode(_ frame: Data) throws -> Data {
        guard !frame.isEmpty, frame.count <= NoiseWire.maxRecord else {
            throw NoiseError.recordTooLarge(frame.count)
        }
        var out = Data(capacity: frame.count + 2)
        out.append(UInt8(truncatingIfNeeded: frame.count >> 8))
        out.append(UInt8(truncatingIfNeeded: frame.count))
        out.append(frame)
        return out
    }

    /// Reads one frame off the front of a buffer, returning it and whatever is
    /// left. `nil` means the buffer does not hold a whole frame yet.
    public static func decode(from buffer: Data) throws -> (frame: Data, rest: Data)? {
        guard buffer.count >= 2 else { return nil }
        let bytes = [UInt8](buffer.prefix(2))
        let length = Int(bytes[0]) << 8 | Int(bytes[1])
        guard length > 0, length <= NoiseWire.maxRecord else {
            throw NoiseError.recordTooLarge(length)
        }
        guard buffer.count >= 2 + length else { return nil }
        return (Data(buffer.dropFirst(2).prefix(length)), Data(buffer.dropFirst(2 + length)))
    }
}

// MARK: - Pinned connection

/// A duplex byte channel carrying length-prefixed Noise frames.
///
/// Keeping this abstract is what lets the handshake be tested end to end with
/// no socket: the LAN and tunnel transports conform to it later, and the tests
/// conform an in-memory pipe.
public protocol NoiseFrameChannel: Sendable {
    func readFrame() async throws -> Data
    func writeFrame(_ frame: Data) async throws
}

/// Runs the XX initiator against a peer whose identity is already known.
public enum NoiseXX {
    /// Handshakes as the initiator and verifies the responder's static key
    /// against `pinnedPeerPublicKey`.
    ///
    /// The pin must be the phone's own record — `PairedDeviceRecord.mac.publicKey`,
    /// the value the QR code moved from the Mac's screen to this phone. It is
    /// deliberately *not* `peerIdentityKey` from a rendezvous answer: that is
    /// the control plane's claim about which Mac this is, and a control plane
    /// that can substitute the key it is checked against is not a check.
    public static func connect(
        channel: NoiseFrameChannel,
        staticKey: Curve25519.KeyAgreement.PrivateKey,
        pinnedPeerPublicKey: String,
        prologue: Data = Data()
    ) async throws -> NoiseSession {
        let expected = try normalizedPin(pinnedPeerPublicKey)
        let handshake = NoiseHandshake(role: .initiator, staticKey: staticKey, prologue: prologue)
        try await channel.writeFrame(try handshake.writeMessage())
        try handshake.readMessage(try await channel.readFrame())
        try await channel.writeFrame(try handshake.writeMessage())
        let session = try handshake.split()
        // Verified after the transcript is complete but before a single byte
        // of gateway traffic moves, and by comparing the key the handshake
        // proved the peer holds against the key pairing pinned. A mismatch
        // ends here; there is no retry and no downgrade.
        guard session.peerStaticKey == expected else {
            throw NoiseError.peerIdentityMismatch(
                expected: expected,
                received: session.peerStaticKey
            )
        }
        return session
    }

    /// Lowercases and length-checks a pin, so a malformed record fails with a
    /// sentence rather than silently never matching.
    public static func normalizedPin(_ value: String) throws -> String {
        let lowered = value.lowercased()
        guard HexCoding.isKey(lowered, bytes: 32) else {
            throw NoiseError.unusablePin("\"\(value)\" is not 32 bytes of hex.")
        }
        return lowered
    }
}

public extension PairedDeviceRecord {
    /// The key every connection from this phone is verified against.
    func pinnedMacPublicKey() throws -> String {
        try NoiseXX.normalizedPin(mac.publicKey)
    }
}
