import CryptoKit
import Foundation

/// How this installation's private key is protected at rest.
///
/// The remote-access transport is `Noise_XX_25519_ChaChaPoly_BLAKE2s`, so the
/// long-lived device identity has to be an X25519 key. The Secure Enclave does
/// not hold X25519 keys — it holds P-256 — so "in the Secure Enclave" is not
/// available literally. What is available, and what this does, is to keep the
/// X25519 private key encrypted under a non-exportable Secure Enclave P-256
/// key, so the ciphertext in the keychain is inert on any other device and
/// unwrappable only by this hardware. Where the Enclave is missing (simulator,
/// older hardware) the key falls back to the keychain alone.
public enum DeviceKeyProtection: String, Codable, Equatable, Sendable {
    /// The X25519 private key is sealed to a non-exportable Secure Enclave key.
    case secureEnclave
    /// The X25519 private key is in the keychain, this device only.
    case keychain

    /// What Settings shows about the strength of the protection.
    public var label: String {
        switch self {
        case .secureEnclave: return "Secure Enclave"
        case .keychain: return "Keychain (this device only)"
        }
    }
}

/// The public half of this installation's long-lived identity.
///
/// Only the public key leaves the phone. It is lowercase hex of the 32-byte
/// X25519 public key, which is the encoding the Mac's pairing record and the
/// Noise handshake both use.
public struct DeviceIdentity: Codable, Equatable, Sendable {
    /// Lowercase hex of the 32-byte X25519 public key.
    public let publicKey: String
    /// How the matching private key is protected.
    public let protection: DeviceKeyProtection
    /// When the identity was created, for display.
    public let createdAt: Date

    public init(publicKey: String, protection: DeviceKeyProtection, createdAt: Date = Date()) {
        self.publicKey = publicKey
        self.protection = protection
        self.createdAt = createdAt
    }

    /// The first and last four hex characters, which is what a person can
    /// realistically compare on two screens.
    public var shortFingerprint: String {
        HexCoding.abbreviate(publicKey)
    }
}

/// Everything that can go wrong holding on to the device identity.
public enum DeviceIdentityError: Error, Equatable, Sendable {
    /// The keychain refused a read, write, or delete.
    case keychain(status: Int32, operation: String)
    /// Stored material exists but is not a usable identity.
    case corruptedIdentity(String)
    /// Key generation itself failed.
    case generationFailed(String)

    public var message: String {
        switch self {
        case .keychain(let status, let operation):
            return "The keychain could not \(operation) this phone's identity (\(status))."
        case .corruptedIdentity(let detail):
            return "This phone's stored identity is unusable: \(detail)"
        case .generationFailed(let detail):
            return "This phone could not create a device identity: \(detail)"
        }
    }
}

/// Storage for the one long-lived identity this installation owns.
///
/// The private key never leaves the conforming type: callers get the public
/// key for registration and ask for signatures or key agreement by name. That
/// keeps a future Noise handshake the only code path that touches the secret.
public protocol DeviceIdentityStoring: Sendable {
    /// The stored identity, or `nil` before the first launch created one.
    func load() throws -> DeviceIdentity?
    /// The stored identity, creating one on first use.
    @discardableResult
    func loadOrCreate() throws -> DeviceIdentity
    /// The private key, for the transport handshake.
    func privateKey() throws -> Curve25519.KeyAgreement.PrivateKey
    /// Removes the identity. The phone is unpairable afterwards until a new
    /// one is created, which is the point: it is the local half of a revoke.
    func destroy() throws
}

/// The app's identity store: keychain-resident, Secure-Enclave-wrapped when
/// the hardware offers it.
public struct KeychainDeviceIdentityStore: DeviceIdentityStoring {
    /// The shape written to the keychain. Only `sealed` is secret; the rest
    /// is metadata kept alongside it so the two can never get out of step.
    struct StoredIdentity: Codable {
        var publicKey: String
        var protection: DeviceKeyProtection
        var createdAt: Date
        /// Raw X25519 private key bytes when `protection == .keychain`, or the
        /// ChaChaPoly box of those bytes when `.secureEnclave`.
        var sealed: Data
        /// The Secure Enclave wrapping key, in its sealed representation. Only
        /// present for `.secureEnclave`.
        var wrappingKey: Data?
        /// The ephemeral P-256 public key the wrapping ECDH used.
        var wrappingEphemeralPublicKey: Data?
    }

    private let service: String
    private let account: String
    private let enclave: SecureEnclaveWrapping

    public init(
        service: String = "dev.cooperativ.latch.mobile",
        account: String = "device-identity"
    ) {
        self.init(service: service, account: account, enclave: SecureEnclaveKeyWrap())
    }

    init(service: String, account: String, enclave: SecureEnclaveWrapping) {
        self.service = service
        self.account = account
        self.enclave = enclave
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }

    public func load() throws -> DeviceIdentity? {
        guard let stored = try read() else { return nil }
        return DeviceIdentity(
            publicKey: stored.publicKey,
            protection: stored.protection,
            createdAt: stored.createdAt
        )
    }

    @discardableResult
    public func loadOrCreate() throws -> DeviceIdentity {
        if let existing = try load() { return existing }
        let key = Curve25519.KeyAgreement.PrivateKey()
        let publicKey = HexCoding.encode(key.publicKey.rawRepresentation)
        let stored: StoredIdentity
        // The Enclave is preferred but never required: a simulator has none,
        // and refusing to pair there would make the app untestable. The
        // difference is recorded and shown rather than hidden.
        if let wrapped = try? enclave.wrap(key.rawRepresentation) {
            stored = StoredIdentity(
                publicKey: publicKey,
                protection: .secureEnclave,
                createdAt: Date(),
                sealed: wrapped.ciphertext,
                wrappingKey: wrapped.wrappingKey,
                wrappingEphemeralPublicKey: wrapped.ephemeralPublicKey
            )
        } else {
            stored = StoredIdentity(
                publicKey: publicKey,
                protection: .keychain,
                createdAt: Date(),
                sealed: key.rawRepresentation,
                wrappingKey: nil,
                wrappingEphemeralPublicKey: nil
            )
        }
        try write(stored)
        return DeviceIdentity(
            publicKey: stored.publicKey,
            protection: stored.protection,
            createdAt: stored.createdAt
        )
    }

    public func privateKey() throws -> Curve25519.KeyAgreement.PrivateKey {
        guard let stored = try read() else {
            throw DeviceIdentityError.corruptedIdentity("there is no identity on this phone")
        }
        return try Self.privateKey(from: stored, enclave: enclave)
    }

    static func privateKey(
        from stored: StoredIdentity,
        enclave: SecureEnclaveWrapping
    ) throws -> Curve25519.KeyAgreement.PrivateKey {
        let raw: Data
        switch stored.protection {
        case .keychain:
            raw = stored.sealed
        case .secureEnclave:
            guard
                let wrappingKey = stored.wrappingKey,
                let ephemeral = stored.wrappingEphemeralPublicKey
            else {
                throw DeviceIdentityError.corruptedIdentity(
                    "the enclave-protected identity is missing its wrapping key"
                )
            }
            raw = try enclave.unwrap(
                WrappedKey(
                    ciphertext: stored.sealed,
                    wrappingKey: wrappingKey,
                    ephemeralPublicKey: ephemeral
                )
            )
        }
        do {
            let key = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
            guard HexCoding.encode(key.publicKey.rawRepresentation) == stored.publicKey else {
                throw DeviceIdentityError.corruptedIdentity(
                    "the stored private key does not match its public key"
                )
            }
            return key
        } catch let error as DeviceIdentityError {
            throw error
        } catch {
            throw DeviceIdentityError.corruptedIdentity(String(describing: error))
        }
    }

    public func destroy() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw DeviceIdentityError.keychain(status: status, operation: "remove")
        }
    }

    private func read() throws -> StoredIdentity? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw DeviceIdentityError.keychain(status: status, operation: "read")
        }
        do {
            return try JSONDecoder().decode(StoredIdentity.self, from: data)
        } catch {
            throw DeviceIdentityError.corruptedIdentity(String(describing: error))
        }
    }

    private func write(_ stored: StoredIdentity) throws {
        let data = try JSONEncoder().encode(stored)
        SecItemDelete(query as CFDictionary)
        var request = query
        request[kSecValueData as String] = data
        // The identity is worthless on another phone and must not sync: it is
        // one half of a pairing the Mac pinned to this hardware.
        request[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(request as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw DeviceIdentityError.keychain(status: status, operation: "save")
        }
    }
}

/// An identity held only in memory, for tests and previews.
public final class MemoryDeviceIdentityStore: DeviceIdentityStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var key: Curve25519.KeyAgreement.PrivateKey?
    private var identity: DeviceIdentity?
    private let protection: DeviceKeyProtection

    public init(protection: DeviceKeyProtection = .secureEnclave) {
        self.protection = protection
    }

    public func load() throws -> DeviceIdentity? {
        lock.lock()
        defer { lock.unlock() }
        return identity
    }

    @discardableResult
    public func loadOrCreate() throws -> DeviceIdentity {
        lock.lock()
        defer { lock.unlock() }
        if let identity { return identity }
        let created = Curve25519.KeyAgreement.PrivateKey()
        let identity = DeviceIdentity(
            publicKey: HexCoding.encode(created.publicKey.rawRepresentation),
            protection: protection
        )
        key = created
        self.identity = identity
        return identity
    }

    public func privateKey() throws -> Curve25519.KeyAgreement.PrivateKey {
        lock.lock()
        defer { lock.unlock() }
        guard let key else {
            throw DeviceIdentityError.corruptedIdentity("there is no identity on this phone")
        }
        return key
    }

    public func destroy() throws {
        lock.lock()
        defer { lock.unlock() }
        key = nil
        identity = nil
    }
}

// MARK: - Secure Enclave wrapping

/// One X25519 private key sealed under a Secure Enclave P-256 key.
struct WrappedKey: Equatable {
    /// The ChaChaPoly box of the X25519 private key bytes.
    var ciphertext: Data
    /// The Enclave key in the representation only this device can restore.
    var wrappingKey: Data
    /// The ephemeral P-256 public key whose agreement produced the box key.
    var ephemeralPublicKey: Data
}

/// The Enclave operations the identity store needs, behind a protocol so the
/// store's fallback and unwrap paths are testable without the hardware.
protocol SecureEnclaveWrapping: Sendable {
    func wrap(_ secret: Data) throws -> WrappedKey
    func unwrap(_ wrapped: WrappedKey) throws -> Data
}

/// The real thing: a non-exportable P-256 key in the Secure Enclave, used to
/// derive a symmetric key by agreement with a per-identity ephemeral key.
struct SecureEnclaveKeyWrap: SecureEnclaveWrapping {
    /// Bound into the derived key so this box cannot be reinterpreted as any
    /// other Latch ciphertext.
    static let info = Data("latch-mobile-device-identity-v1".utf8)

    func wrap(_ secret: Data) throws -> WrappedKey {
        guard SecureEnclave.isAvailable else {
            throw DeviceIdentityError.generationFailed("this device has no Secure Enclave")
        }
        do {
            let enclaveKey = try SecureEnclave.P256.KeyAgreement.PrivateKey()
            let ephemeral = P256.KeyAgreement.PrivateKey()
            let shared = try ephemeral.sharedSecretFromKeyAgreement(with: enclaveKey.publicKey)
            let box = try ChaChaPoly.seal(secret, using: Self.symmetricKey(from: shared))
            return WrappedKey(
                ciphertext: box.combined,
                wrappingKey: enclaveKey.dataRepresentation,
                ephemeralPublicKey: ephemeral.publicKey.rawRepresentation
            )
        } catch {
            throw DeviceIdentityError.generationFailed(String(describing: error))
        }
    }

    func unwrap(_ wrapped: WrappedKey) throws -> Data {
        do {
            let enclaveKey = try SecureEnclave.P256.KeyAgreement.PrivateKey(
                dataRepresentation: wrapped.wrappingKey
            )
            let ephemeral = try P256.KeyAgreement.PublicKey(
                rawRepresentation: wrapped.ephemeralPublicKey
            )
            let shared = try enclaveKey.sharedSecretFromKeyAgreement(with: ephemeral)
            let box = try ChaChaPoly.SealedBox(combined: wrapped.ciphertext)
            return try ChaChaPoly.open(box, using: Self.symmetricKey(from: shared))
        } catch {
            throw DeviceIdentityError.corruptedIdentity(String(describing: error))
        }
    }

    static func symmetricKey(from shared: SharedSecret) -> SymmetricKey {
        shared.hkdfDerivedSymmetricKey(
            using: SHA256.self,
            salt: Data(),
            sharedInfo: info,
            outputByteCount: 32
        )
    }
}

// MARK: - Hex

/// Lowercase hex, which is how every key in the remote-access contract is
/// spelled on the wire and in the Mac's pairing records.
public enum HexCoding {
    public static func encode(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Decodes an even-length hex string, rejecting anything else. Uppercase
    /// is accepted on the way in because a QR code is not ours to police.
    public static func decode(_ value: String) -> Data? {
        guard value.count % 2 == 0 else { return nil }
        var bytes = Data(capacity: value.count / 2)
        var high: UInt8?
        for character in value.utf8 {
            guard let nibble = nibble(character) else { return nil }
            if let start = high {
                bytes.append(start << 4 | nibble)
                high = nil
            } else {
                high = nibble
            }
        }
        return high == nil ? bytes : nil
    }

    /// Whether a string is hex of exactly `bytes` bytes.
    public static func isKey(_ value: String, bytes: Int) -> Bool {
        value.count == bytes * 2 && decode(value) != nil
    }

    /// `a1b2…9f8e`, for showing a key to a person.
    public static func abbreviate(_ value: String) -> String {
        guard value.count > 12 else { return value }
        return "\(value.prefix(4))…\(value.suffix(4))"
    }

    private static func nibble(_ byte: UInt8) -> UInt8? {
        switch byte {
        case UInt8(ascii: "0")...UInt8(ascii: "9"): return byte - UInt8(ascii: "0")
        case UInt8(ascii: "a")...UInt8(ascii: "f"): return byte - UInt8(ascii: "a") + 10
        case UInt8(ascii: "A")...UInt8(ascii: "F"): return byte - UInt8(ascii: "A") + 10
        default: return nil
        }
    }
}
