import Foundation

/// Where the link to the user's computer is kept between launches.
///
/// The token is a bearer credential for a live agent session, so it belongs in
/// the keychain rather than `UserDefaults`. The address is not a secret, but it
/// is stored alongside the token so the two cannot get out of step.
public protocol LinkStorage: Sendable {
    func load() throws -> GatewayLink?
    func save(_ link: GatewayLink) throws
    func clear() throws
}

/// Keychain-backed storage, used by the app.
public struct KeychainLinkStorage: LinkStorage {
    private let service: String
    private let account: String

    public init(
        service: String = "dev.cooperativ.latch.mobile",
        account: String = "gateway-link"
    ) {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }

    public func load() throws -> GatewayLink? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw LatchError.transport("could not read the saved link (\(status))")
        }
        return try? JSONDecoder().decode(GatewayLink.self, from: data)
    }

    public func save(_ link: GatewayLink) throws {
        let data = try JSONEncoder().encode(link)
        // The item is rewritten rather than updated so a change of address and
        // token always lands as one value.
        SecItemDelete(query as CFDictionary)
        var request = query
        request[kSecValueData as String] = data
        // The link is only useful while the phone is unlocked and is never
        // synced to another device: it points at one specific computer.
        request[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(request as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw LatchError.transport("could not save the link (\(status))")
        }
    }

    public func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw LatchError.transport("could not remove the link (\(status))")
        }
    }
}

/// In-memory storage, for tests and previews.
public final class MemoryLinkStorage: LinkStorage, @unchecked Sendable {
    private let lock = NSLock()
    private var link: GatewayLink?

    public init(link: GatewayLink? = nil) {
        self.link = link
    }

    public func load() throws -> GatewayLink? {
        lock.lock()
        defer { lock.unlock() }
        return link
    }

    public func save(_ link: GatewayLink) throws {
        lock.lock()
        defer { lock.unlock() }
        self.link = link
    }

    public func clear() throws {
        lock.lock()
        defer { lock.unlock() }
        link = nil
    }
}
