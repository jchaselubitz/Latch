import CryptoKit
import Foundation
import Security

/// This device's notification key: the X25519 key the Mac seals attention
/// alerts to.
///
/// It is deliberately not the Noise identity key. The notification service
/// extension has to open alerts while the phone is locked, so this key is
/// stored `AfterFirstUnlockThisDeviceOnly` in a keychain access group the
/// extension shares; the identity key stays `WhenUnlocked` and app-only. The
/// worst a stolen notification key reveals is the names of sessions that
/// asked for attention — never transport access.
public struct NotificationKeyStore: Sendable {
    /// Storage for the raw key, injectable so tests need no keychain.
    public struct Storage: Sendable {
        public var read: @Sendable () -> Data?
        public var write: @Sendable (Data) -> Bool

        public init(read: @escaping @Sendable () -> Data?, write: @escaping @Sendable (Data) -> Bool) {
            self.read = read
            self.write = write
        }
    }

    public static let service = "dev.cooperativ.latch.mobile"
    public static let account = "notification-key"

    private let storage: Storage

    public init(storage: Storage) {
        self.storage = storage
    }

    /// The keychain in `accessGroup` — the group the app and its extension
    /// share — or the default group when nil.
    public static func keychain(accessGroup: String?) -> NotificationKeyStore {
        NotificationKeyStore(storage: .keychain(accessGroup: accessGroup))
    }

    /// The stored key, if one exists and is well-formed.
    public func load() -> Curve25519.KeyAgreement.PrivateKey? {
        guard let raw = storage.read() else { return nil }
        return try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
    }

    /// The stored key, creating and storing one first if there is none.
    /// Nil only when storage refuses the write.
    public func loadOrCreate() -> Curve25519.KeyAgreement.PrivateKey? {
        if let existing = load() { return existing }
        let key = Curve25519.KeyAgreement.PrivateKey()
        return storage.write(key.rawRepresentation) ? key : nil
    }

    /// The public half as the Mac expects it: 32 bytes, lowercase hex.
    public static func publicKeyHex(_ key: Curve25519.KeyAgreement.PrivateKey) -> String {
        key.publicKey.rawRepresentation.lowercaseHex
    }
}

extension NotificationKeyStore.Storage {
    static func keychain(accessGroup: String?) -> Self {
        Self(
            read: {
                var query = keychainQuery(accessGroup: accessGroup)
                query[kSecReturnData as String] = true
                query[kSecMatchLimit as String] = kSecMatchLimitOne
                var result: CFTypeRef?
                guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
                return result as? Data
            },
            write: { data in
                let query = keychainQuery(accessGroup: accessGroup)
                let attributes: [String: Any] = [
                    kSecValueData as String: data,
                    kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                ]
                let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
                if status == errSecSuccess { return true }
                guard status == errSecItemNotFound else { return false }
                let added = query.merging(attributes) { _, new in new }
                return SecItemAdd(added as CFDictionary, nil) == errSecSuccess
            }
        )
    }
}

private func keychainQuery(accessGroup: String?) -> [String: Any] {
    var query: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: NotificationKeyStore.service,
        kSecAttrAccount as String: NotificationKeyStore.account,
    ]
    if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
    return query
}
