import Foundation

/// What a paired phone is allowed to do, as the Mac stores it.
///
/// This mirrors the desktop's `DevicePermission`. The phone never decides its
/// own permission — it displays the one the Mac granted, so that a screen
/// missing a control has a stated reason rather than looking broken.
public enum DevicePermission: String, Codable, CaseIterable, Equatable, Sendable {
    /// List sessions and read events or terminal output.
    case observe
    /// Also send messages and answer prompts.
    case interact
    /// Also send terminal bytes and resize.
    case control

    /// What the Mac grants a newly paired phone.
    public static let `default` = DevicePermission.interact

    public var label: String {
        switch self {
        case .observe: return "Watch only"
        case .interact: return "Watch and reply"
        case .control: return "Full terminal control"
        }
    }

    public var explanation: String {
        switch self {
        case .observe:
            return "This phone can read sessions and transcripts. Sending is off."
        case .interact:
            return "This phone can read sessions and send messages and prompt answers."
        case .control:
            return "This phone can also type into the terminal and resize it."
        }
    }

    /// Whether this grant covers an operation needing `required`.
    public func permits(_ required: DevicePermission) -> Bool {
        switch (self, required) {
        case (.control, _): return true
        case (.interact, .interact), (.interact, .observe): return true
        case (.observe, .observe): return true
        default: return false
        }
    }
}

/// The Mac this phone is paired with, as pinned at pairing time.
public struct PairedMac: Codable, Equatable, Sendable {
    /// Opaque identifier the control plane uses for the Mac.
    public let deviceId: String?
    /// The pinned identity. Every later connection verifies the peer static
    /// key against this value; a mismatch is an impersonation, not a retry.
    public let publicKey: String
    /// Display name. Advisory only — the key is the identity.
    public let name: String?

    public init(deviceId: String? = nil, publicKey: String, name: String? = nil) {
        self.deviceId = deviceId
        self.publicKey = publicKey
        self.name = name
    }

    /// What to show as the Mac's title.
    public var displayName: String {
        if let name, !name.isEmpty { return name }
        return "Mac \(HexCoding.abbreviate(publicKey))"
    }

    public var shortFingerprint: String {
        HexCoding.abbreviate(publicKey)
    }
}

/// The record this phone keeps after a successful pairing.
///
/// It holds a bearer token for the control plane, so it belongs in the
/// keychain rather than `UserDefaults` — the same rule the gateway link
/// follows. The pairing secret is deliberately absent: it is one-time, it was
/// consumed by enrollment, and keeping it would recreate the thing pairing
/// exists to make short-lived.
public struct PairedDeviceRecord: Codable, Equatable, Sendable {
    /// Opaque identifier the Mac and control plane know this phone by.
    public let deviceId: String
    /// The label shown on the Mac's device list.
    public let name: String
    /// This phone's public identity, as enrolled.
    public let devicePublicKey: String
    /// The Mac this phone is paired with.
    public let mac: PairedMac
    /// The permission the Mac granted.
    public var permission: DevicePermission
    /// Whether the Mac has revoked this phone. A revoked record is kept rather
    /// than deleted so the app can say why it stopped working.
    public var revoked: Bool
    /// The phrase both screens showed, kept so Settings can show it again.
    public let phrase: String
    public let pairedAt: Date
    /// Where to reach the control plane for permission refresh and revocation.
    public let controlPlane: URL?
    /// Short-lived credential for control-plane calls. Never sent to a gateway.
    public var accessToken: String?

    public init(
        deviceId: String,
        name: String,
        devicePublicKey: String,
        mac: PairedMac,
        permission: DevicePermission,
        revoked: Bool = false,
        phrase: String,
        pairedAt: Date = Date(),
        controlPlane: URL? = nil,
        accessToken: String? = nil
    ) {
        self.deviceId = deviceId
        self.name = name
        self.devicePublicKey = devicePublicKey
        self.mac = mac
        self.permission = permission
        self.revoked = revoked
        self.phrase = phrase
        self.pairedAt = pairedAt
        self.controlPlane = controlPlane
        self.accessToken = accessToken
    }

    /// Whether this phone may still connect.
    public var isActive: Bool { !revoked }
}

/// Where the paired-device record lives between launches.
public protocol PairedDeviceStoring: Sendable {
    func load() throws -> PairedDeviceRecord?
    func save(_ record: PairedDeviceRecord) throws
    func clear() throws
}

/// Keychain-backed storage, used by the app.
public struct KeychainPairedDeviceStore: PairedDeviceStoring {
    private let service: String
    private let account: String

    public init(
        service: String = "dev.cooperativ.latch.mobile",
        account: String = "paired-device"
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

    public func load() throws -> PairedDeviceRecord? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw DeviceIdentityError.keychain(status: status, operation: "read")
        }
        return try? JSONDecoder().decode(PairedDeviceRecord.self, from: data)
    }

    public func save(_ record: PairedDeviceRecord) throws {
        let data = try JSONEncoder().encode(record)
        // Rewritten rather than updated so the token and the identity it
        // belongs to always land as one value.
        SecItemDelete(query as CFDictionary)
        var request = query
        request[kSecValueData as String] = data
        request[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(request as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw DeviceIdentityError.keychain(status: status, operation: "save")
        }
    }

    public func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw DeviceIdentityError.keychain(status: status, operation: "remove")
        }
    }
}

/// In-memory storage, for tests and previews.
public final class MemoryPairedDeviceStore: PairedDeviceStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var record: PairedDeviceRecord?

    public init(record: PairedDeviceRecord? = nil) {
        self.record = record
    }

    public func load() throws -> PairedDeviceRecord? {
        lock.lock()
        defer { lock.unlock() }
        return record
    }

    public func save(_ record: PairedDeviceRecord) throws {
        lock.lock()
        defer { lock.unlock() }
        self.record = record
    }

    public func clear() throws {
        lock.lock()
        defer { lock.unlock() }
        record = nil
    }
}
