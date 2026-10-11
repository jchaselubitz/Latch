import CryptoKit
import Foundation

/// What a sealed attention alert says, once opened on this phone.
///
/// The Mac seals it with HPKE (DHKEM X25519/HKDF-SHA256, HKDF-SHA256,
/// ChaCha20-Poly1305) to this device's notification key, binding it to the
/// alert's event id; the control plane forwards the bytes without being able
/// to read them. See `crates/latch/src/cli/serve/attention/seal.rs`, whose
/// `interop` vector `AttentionPayloadTests` also opens.
public struct AttentionPayload: Decodable, Equatable, Sendable {
    public enum Kind: String, Decodable, Sendable {
        /// The agent is waiting on a permission, a question, or input.
        case approval
        /// The agent finished its turn.
        case completion
    }

    /// Session id, which a tap opens.
    public let sessionID: String
    /// Session name.
    public let name: String
    /// Session title, when it has one.
    public let title: String?
    public let kind: Kind

    enum CodingKeys: String, CodingKey {
        case version = "v"
        case sessionID = "s"
        case name = "n"
        case title = "t"
        case kind = "k"
    }

    public init(sessionID: String, name: String, title: String?, kind: Kind) {
        self.sessionID = sessionID
        self.name = name
        self.title = title
        self.kind = kind
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard try container.decode(Int.self, forKey: .version) == 1 else {
            throw AttentionPayloadError.unsupportedVersion
        }
        sessionID = try container.decode(String.self, forKey: .sessionID)
        name = try container.decode(String.self, forKey: .name)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        kind = try container.decode(Kind.self, forKey: .kind)
    }

    /// HPKE `info`, identical to the Mac's.
    public static let info = Data("latch-attention/v1".utf8)
    /// Upper bound on a sealed payload, mirroring the control plane's.
    public static let maxSealedHexLength = 2048

    /// Opens `sealedHex` — the encapsulated key followed by the ciphertext —
    /// for `eventID` with this device's notification key.
    public static func open(
        sealedHex: String,
        eventID: String,
        privateKey: Curve25519.KeyAgreement.PrivateKey
    ) throws -> AttentionPayload {
        guard sealedHex.count <= maxSealedHexLength, let wire = Data(hex: sealedHex), wire.count > 32 + 16 else {
            throw AttentionPayloadError.malformed
        }
        let encapsulatedKey = wire.prefix(32)
        let ciphertext = wire.dropFirst(32)
        var recipient = try HPKE.Recipient(
            privateKey: privateKey,
            ciphersuite: .Curve25519_SHA256_ChachaPoly,
            info: info,
            encapsulatedKey: Data(encapsulatedKey)
        )
        let plaintext = try recipient.open(Data(ciphertext), authenticating: Data(eventID.utf8))
        return try JSONDecoder().decode(AttentionPayload.self, from: plaintext)
    }
}

public enum AttentionPayloadError: Error, Equatable {
    case malformed
    case unsupportedVersion
}

extension Data {
    /// Lowercase or uppercase hex, even length; nil otherwise.
    init?(hex: String) {
        guard hex.count % 2 == 0 else { return nil }
        var bytes = [UInt8]()
        bytes.reserveCapacity(hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        self.init(bytes)
    }

    var lowercaseHex: String { map { String(format: "%02x", $0) }.joined() }
}
