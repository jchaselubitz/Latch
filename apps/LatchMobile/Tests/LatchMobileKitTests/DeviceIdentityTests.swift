import CryptoKit
import XCTest

@testable import LatchMobileKit

/// A stand-in for the Secure Enclave, so the wrapping path and the fallback
/// can both be exercised on a machine that has no Enclave (and without asking
/// the real keychain for anything).
private struct StubEnclave: SecureEnclaveWrapping {
    enum Behavior: Sendable, Equatable {
        /// Wraps with a fixed symmetric key.
        case available
        /// Refuses to wrap, as an older device or a simulator does.
        case unavailable
    }

    let behavior: Behavior
    static let key = SymmetricKey(size: .bits256)

    func wrap(_ secret: Data) throws -> WrappedKey {
        guard behavior == .available else {
            throw DeviceIdentityError.generationFailed("no Secure Enclave")
        }
        let box = try ChaChaPoly.seal(secret, using: Self.key)
        return WrappedKey(
            ciphertext: box.combined,
            wrappingKey: Data("enclave-key".utf8),
            ephemeralPublicKey: Data("ephemeral".utf8)
        )
    }

    func unwrap(_ wrapped: WrappedKey) throws -> Data {
        do {
            let box = try ChaChaPoly.SealedBox(combined: wrapped.ciphertext)
            return try ChaChaPoly.open(box, using: Self.key)
        } catch {
            throw DeviceIdentityError.corruptedIdentity(String(describing: error))
        }
    }
}

final class DeviceIdentityTests: XCTestCase {
    private typealias Stored = KeychainDeviceIdentityStore.StoredIdentity

    private func stored(protection: DeviceKeyProtection, enclave: StubEnclave) throws -> (Stored, Curve25519.KeyAgreement.PrivateKey) {
        let key = Curve25519.KeyAgreement.PrivateKey()
        switch protection {
        case .secureEnclave:
            let wrapped = try enclave.wrap(key.rawRepresentation)
            return (
                Stored(
                    publicKey: HexCoding.encode(key.publicKey.rawRepresentation),
                    protection: .secureEnclave,
                    createdAt: Date(),
                    sealed: wrapped.ciphertext,
                    wrappingKey: wrapped.wrappingKey,
                    wrappingEphemeralPublicKey: wrapped.ephemeralPublicKey
                ),
                key
            )
        case .keychain:
            return (
                Stored(
                    publicKey: HexCoding.encode(key.publicKey.rawRepresentation),
                    protection: .keychain,
                    createdAt: Date(),
                    sealed: key.rawRepresentation,
                    wrappingKey: nil,
                    wrappingEphemeralPublicKey: nil
                ),
                key
            )
        }
    }

    /// The enclave-wrapped key comes back as the same key it went in as.
    func testEnclaveWrappedIdentityRoundTrips() throws {
        let enclave = StubEnclave(behavior: .available)
        let (record, key) = try stored(protection: .secureEnclave, enclave: enclave)
        // The private key must not be sitting in the stored blob in the clear.
        XCTAssertNotEqual(record.sealed, key.rawRepresentation)
        let restored = try KeychainDeviceIdentityStore.privateKey(from: record, enclave: enclave)
        XCTAssertEqual(restored.rawRepresentation, key.rawRepresentation)
    }

    /// Without the Enclave the key is still usable, and the difference is
    /// recorded rather than hidden.
    func testKeychainFallbackIdentityRoundTrips() throws {
        let enclave = StubEnclave(behavior: .unavailable)
        let (record, key) = try stored(protection: .keychain, enclave: enclave)
        XCTAssertEqual(record.protection, .keychain)
        let restored = try KeychainDeviceIdentityStore.privateKey(from: record, enclave: enclave)
        XCTAssertEqual(restored.rawRepresentation, key.rawRepresentation)
    }

    func testEnclaveRecordMissingItsWrappingKeyIsRefused() throws {
        let enclave = StubEnclave(behavior: .available)
        var (record, _) = try stored(protection: .secureEnclave, enclave: enclave)
        record.wrappingKey = nil
        XCTAssertThrowsError(
            try KeychainDeviceIdentityStore.privateKey(from: record, enclave: enclave)
        ) { error in
            guard case .corruptedIdentity = error as? DeviceIdentityError else {
                return XCTFail("expected a corrupted identity, got \(error)")
            }
        }
    }

    func testTamperedCiphertextIsRefused() throws {
        let enclave = StubEnclave(behavior: .available)
        var (record, _) = try stored(protection: .secureEnclave, enclave: enclave)
        record.sealed[record.sealed.index(before: record.sealed.endIndex)] ^= 0xff
        XCTAssertThrowsError(
            try KeychainDeviceIdentityStore.privateKey(from: record, enclave: enclave)
        )
    }

    /// A private key that does not belong to the stored public key would make
    /// the phone announce an identity it cannot authenticate as.
    func testMismatchedPublicKeyIsRefused() throws {
        let enclave = StubEnclave(behavior: .unavailable)
        var (record, _) = try stored(protection: .keychain, enclave: enclave)
        record.publicKey = String(repeating: "0", count: 64)
        XCTAssertThrowsError(
            try KeychainDeviceIdentityStore.privateKey(from: record, enclave: enclave)
        ) { error in
            guard case .corruptedIdentity = error as? DeviceIdentityError else {
                return XCTFail("expected a corrupted identity, got \(error)")
            }
        }
    }

    /// The Mac's `decode_static_key` wants exactly 32 bytes of hex, so what
    /// this phone enrolls has to be exactly that.
    func testIdentityIsA32ByteHexKey() throws {
        let store = MemoryDeviceIdentityStore()
        let identity = try store.loadOrCreate()
        XCTAssertTrue(HexCoding.isKey(identity.publicKey, bytes: 32))
        XCTAssertEqual(identity.publicKey, identity.publicKey.lowercased())
    }

    func testIdentityIsCreatedOnceAndReused() throws {
        let store = MemoryDeviceIdentityStore()
        let first = try store.loadOrCreate()
        XCTAssertEqual(try store.loadOrCreate(), first)
        XCTAssertEqual(try store.load(), first)

        try store.destroy()
        XCTAssertNil(try store.load())
        XCTAssertNotEqual(try store.loadOrCreate().publicKey, first.publicKey)
    }

    func testHexRoundTripsAndRejectsRubbish() {
        XCTAssertEqual(HexCoding.encode(Data([0x00, 0x0f, 0xff])), "000fff")
        XCTAssertEqual(HexCoding.decode("000FFF"), Data([0x00, 0x0f, 0xff]))
        XCTAssertNil(HexCoding.decode("abc"))
        XCTAssertNil(HexCoding.decode("zz"))
        XCTAssertEqual(HexCoding.decode(""), Data())
        XCTAssertFalse(HexCoding.isKey(String(repeating: "a", count: 63), bytes: 32))
        XCTAssertTrue(HexCoding.isKey(String(repeating: "a", count: 64), bytes: 32))
        XCTAssertEqual(HexCoding.abbreviate("0123456789abcdef"), "0123…cdef")
        XCTAssertEqual(HexCoding.abbreviate("short"), "short")
    }
}
