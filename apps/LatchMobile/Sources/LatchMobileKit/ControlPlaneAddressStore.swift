import Foundation

/// Reading an address a person typed.
public enum ControlPlaneAddress {
    /// Parses a typed address.
    ///
    /// `latch.example.com` is accepted as shorthand for
    /// `https://latch.example.com`, because that is what a person reads off a
    /// screen. Anything that is not a usable http(s) address is refused rather
    /// than turned into a request that cannot succeed, and a trailing slash is
    /// dropped because every path sent from here is absolute.
    public static func parse(_ typed: String) -> URL? {
        let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        var candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        while candidate.hasSuffix("/") { candidate.removeLast() }
        guard
            let url = URL(string: candidate),
            let scheme = url.scheme?.lowercased(),
            scheme == "https" || scheme == "http",
            let host = url.host,
            !host.isEmpty
        else {
            return nil
        }
        return url
    }
}

/// Where the control-plane address this phone should enroll against is kept
/// between launches.
///
/// This is deliberately not the keychain: the address is a public service
/// endpoint, not a credential, and the credentials it goes on to produce are
/// stored separately by `PairedDeviceStoring`. Keeping it here means a phone
/// that had to be told the address once is not asked again.
public protocol ControlPlaneAddressStoring: Sendable {
    func load() -> URL?
    func save(_ address: URL)
    func clear()
}

/// `UserDefaults`-backed storage, used by the app.
public struct UserDefaultsControlPlaneAddressStore: ControlPlaneAddressStoring {
    private let defaults: UserDefaults
    private let key: String

    public init(
        defaults: UserDefaults = .standard,
        key: String = "controlPlaneAddress"
    ) {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> URL? {
        guard let raw = defaults.string(forKey: key), !raw.isEmpty else { return nil }
        return ControlPlaneAddress.parse(raw)
    }

    public func save(_ address: URL) {
        defaults.set(address.absoluteString, forKey: key)
    }

    public func clear() {
        defaults.removeObject(forKey: key)
    }
}

/// In-memory storage, for tests and previews.
public final class MemoryControlPlaneAddressStore: ControlPlaneAddressStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var address: URL?

    public init(_ address: URL? = nil) {
        self.address = address
    }

    public func load() -> URL? {
        lock.lock()
        defer { lock.unlock() }
        return address
    }

    public func save(_ address: URL) {
        lock.lock()
        defer { lock.unlock() }
        self.address = address
    }

    public func clear() {
        lock.lock()
        defer { lock.unlock() }
        address = nil
    }
}
