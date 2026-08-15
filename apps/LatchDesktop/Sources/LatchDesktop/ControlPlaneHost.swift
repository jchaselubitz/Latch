import Foundation
import CryptoKit
import Security

/// The Mac's half of the control-plane pairing contract.
///
/// The QR code a phone scans carries a one-time secret and the Mac's pinned
/// identity, but the phone still has to be told *where* to present them. That
/// address, and the pairing record the phone's `POST /v1/pairings/:id/confirm`
/// resolves against, both have to come from this side: the CLI has no HTTP
/// client and the every-window startup budget rules out giving it one, so the
/// desktop app is the host adapter.
///
/// Nothing here learns a session, a transcript, or the supervised gateway
/// token. It registers the digest of the pairing secret — never the secret —
/// so a control-plane breach cannot answer a scan on this Mac's behalf, and it
/// reads back only the enrolled phone's public identity so the local device
/// store can be completed. See `docs/REMOTE_ACCESS_THREAT_MODEL.md`.

// MARK: - Stored enrollment

/// This Mac's credentials with one control plane.
///
/// The address is stored with the tokens rather than beside them because they
/// are only meaningful together: a token issued by one deployment names
/// nothing in another, so a changed address has to re-enroll rather than reuse.
struct HostEnrollment: Codable, Equatable, Sendable {
    /// The control plane these credentials belong to, normalized.
    let address: String
    /// Account credential, returned once by `POST /v1/accounts`.
    let accountToken: String
    /// This Mac's opaque control-plane device id.
    let deviceID: String
    /// Device credential, returned once by `POST /v1/devices`.
    let deviceToken: String
    /// The Noise static public key this Mac was enrolled with. A local key
    /// rotation makes the stored record stale, which is detectable only by
    /// keeping the key that was registered.
    let publicKey: String
}

protocol HostEnrollmentStoring: Sendable {
    func load() throws -> HostEnrollment?
    func save(_ enrollment: HostEnrollment) throws
    func clear() throws
}

/// Keychain-backed storage. These are bearer credentials for this Mac's
/// account, so they belong next to the Noise private key rather than in
/// `UserDefaults`.
struct KeychainHostEnrollmentStore: HostEnrollmentStoring {
    private let service: String
    private let account: String

    init(
        service: String = "co.cooperativ.latch.control-plane",
        account: String = "host-enrollment"
    ) {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func load() throws -> HostEnrollment? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw ControlPlaneHostError.storage("could not read this Mac's control-plane credentials (\(status))")
        }
        return try? JSONDecoder().decode(HostEnrollment.self, from: data)
    }

    func save(_ enrollment: HostEnrollment) throws {
        let data = try JSONEncoder().encode(enrollment)
        // Rewritten rather than updated so the address and the tokens it goes
        // with can never land as two separate values.
        SecItemDelete(query as CFDictionary)
        var request = query
        request[kSecValueData as String] = data
        request[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(request as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw ControlPlaneHostError.storage("could not save this Mac's control-plane credentials (\(status))")
        }
    }

    func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw ControlPlaneHostError.storage("could not remove this Mac's control-plane credentials (\(status))")
        }
    }
}

/// In-memory storage, for tests and previews.
final class MemoryHostEnrollmentStore: HostEnrollmentStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var enrollment: HostEnrollment?

    init(_ enrollment: HostEnrollment? = nil) {
        self.enrollment = enrollment
    }

    func load() throws -> HostEnrollment? {
        lock.lock()
        defer { lock.unlock() }
        return enrollment
    }

    func save(_ enrollment: HostEnrollment) throws {
        lock.lock()
        defer { lock.unlock() }
        self.enrollment = enrollment
    }

    func clear() throws {
        lock.lock()
        defer { lock.unlock() }
        enrollment = nil
    }
}

// MARK: - Failures

enum ControlPlaneHostError: LocalizedError, Equatable, Sendable {
    /// No control-plane address has been configured on this Mac.
    case notConfigured
    /// The typed address is not a control-plane address.
    case invalidAddress(String)
    /// Remote access is on but the CLI has not reported this Mac's identity.
    case noIdentity
    /// A non-2xx answer.
    case http(status: Int, path: String, reason: String)
    /// The answer did not match the contract.
    case malformedResponse(String)
    /// The network failed.
    case transport(String)
    /// The Keychain refused.
    case storage(String)

    var errorDescription: String? {
        switch self {
        case .notConfigured:
            return """
            No control plane is set for this Mac, so a scanned code would have nowhere to enroll. \
            Add the address in Remote Access settings.
            """
        case .invalidAddress(let value):
            return "\(value) is not a control-plane address. Use an https:// address."
        case .noIdentity:
            return "This Mac has no remote-access identity yet. Turn remote access on first."
        case .http(let status, let path, let reason):
            return "The control plane refused \(path) (\(status)): \(reason)"
        case .malformedResponse(let detail):
            return "The control plane sent an unexpected response: \(detail)"
        case .transport(let detail):
            return detail
        case .storage(let detail):
            return detail
        }
    }
}

// MARK: - Wire types

/// A device as the control plane describes it. Only the fields this side acts
/// on are modeled; unknown properties are ignored rather than stored.
struct ControlPlaneDevice: Decodable, Equatable, Sendable {
    let deviceID: String
    let name: String
    let role: String
    let publicKey: String
    let revoked: Bool
    let permission: DevicePermission?

    enum CodingKeys: String, CodingKey {
        case name, role, publicKey, revoked, permission
        case deviceID = "deviceId"
    }
}

/// The host-side calls, behind a protocol so pairing is testable without a
/// deployed control plane.
protocol ControlPlaneHostAPI: Sendable {
    func createAccount(label: String) async throws -> String
    func enrollHost(
        accountToken: String,
        name: String,
        publicKey: String
    ) async throws -> (deviceID: String, deviceToken: String)
    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws
    func openPairingRequest(
        deviceToken: String,
        pairingID: String,
        secretDigest: String,
        permission: DevicePermission
    ) async throws
    func devices(deviceToken: String) async throws -> [ControlPlaneDevice]
}

/// The HTTP implementation.
///
/// An actor rather than a struct for the same reason the phone's client is
/// one: `URLSession` is not `Sendable`, and this is handed to a `@MainActor`
/// adapter that awaits it from a background context.
actor HTTPControlPlaneHostAPI: ControlPlaneHostAPI {
    private let baseURL: URL
    private let session: URLSession

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func createAccount(label: String) async throws -> String {
        struct Response: Decodable { let accountToken: String }
        let response: Response = try await send(
            path: "/v1/accounts",
            method: "POST",
            token: nil,
            body: ["label": label]
        )
        return response.accountToken
    }

    func enrollHost(
        accountToken: String,
        name: String,
        publicKey: String
    ) async throws -> (deviceID: String, deviceToken: String) {
        struct Response: Decodable {
            let deviceId: String
            let deviceToken: String
        }
        let response: Response = try await send(
            path: "/v1/devices",
            method: "POST",
            token: accountToken,
            body: [
                "name": name,
                "platform": "macos",
                "role": "host",
                "publicKey": publicKey,
            ]
        )
        return (response.deviceId, response.deviceToken)
    }

    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws {
        let _: Empty = try await send(
            path: "/v1/devices/\(escape(deviceID))/rotate-key",
            method: "POST",
            token: deviceToken,
            body: ["publicKey": publicKey]
        )
    }

    func openPairingRequest(
        deviceToken: String,
        pairingID: String,
        secretDigest: String,
        permission: DevicePermission
    ) async throws {
        // `expiresAt` is deliberately omitted: the control plane applies the
        // same five-minute ceiling, and sending this Mac's absolute deadline
        // would have a clock a second fast rejected as out of range.
        let _: Empty = try await send(
            path: "/v1/pairings/requests",
            method: "POST",
            token: deviceToken,
            body: [
                "pairingId": pairingID,
                "secretDigest": secretDigest,
                "permission": permission.rawValue,
            ]
        )
    }

    func devices(deviceToken: String) async throws -> [ControlPlaneDevice] {
        struct Response: Decodable { let devices: [ControlPlaneDevice] }
        let response: Response = try await send(
            path: "/v1/devices",
            method: "GET",
            token: deviceToken,
            body: nil
        )
        return response.devices
    }

    /// A 204 or an answer this side does not read.
    private struct Empty: Decodable {
        init() {}
        init(from decoder: Decoder) throws {}
    }

    private func escape(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? value
    }

    private func send<Response: Decodable>(
        path: String,
        method: String,
        token: String?,
        body: [String: Any]?
    ) async throws -> Response {
        guard let url = URL(string: baseURL.absoluteString + path) else {
            throw ControlPlaneHostError.transport("\(baseURL.absoluteString + path) is not a usable address")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw ControlPlaneHostError.transport(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw ControlPlaneHostError.malformedResponse("no HTTP status")
        }
        guard (200..<300).contains(http.statusCode) else {
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            let reason = object?["reason"] as? String ?? object?["error"] as? String ?? "request failed"
            throw ControlPlaneHostError.http(status: http.statusCode, path: path, reason: reason)
        }
        if data.isEmpty, let empty = Empty() as? Response {
            return empty
        }
        do {
            return try JSONDecoder().decode(Response.self, from: data)
        } catch {
            throw ControlPlaneHostError.malformedResponse(String(describing: error))
        }
    }
}

// MARK: - The adapter

/// Keeps this Mac enrolled with one control plane and registers the pairing
/// codes it displays.
@MainActor
final class ControlPlaneHost {
    /// Where the address lives between launches. It is not a secret; the
    /// credentials it goes with are the part that is kept in the Keychain.
    static let addressKey = "remoteAccessControlPlane"

    private let store: HostEnrollmentStoring
    private let defaults: UserDefaults
    private let apiFactory: @Sendable (URL) -> ControlPlaneHostAPI

    init(
        store: HostEnrollmentStoring = KeychainHostEnrollmentStore(),
        defaults: UserDefaults = LatchClient.preferences,
        apiFactory: @escaping @Sendable (URL) -> ControlPlaneHostAPI = { HTTPControlPlaneHostAPI(baseURL: $0) }
    ) {
        self.store = store
        self.defaults = defaults
        self.apiFactory = apiFactory
    }

    /// The configured control plane, if there is one.
    var address: URL? {
        guard let raw = defaults.string(forKey: Self.addressKey), !raw.isEmpty else { return nil }
        return try? Self.normalize(raw)
    }

    var isConfigured: Bool { address != nil }

    /// Accepts an address the user typed.
    ///
    /// A blank value clears the setting, which is the supported way to go back
    /// to a Mac that pairs over the local network only.
    func setAddress(_ raw: String) throws {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            defaults.removeObject(forKey: Self.addressKey)
            return
        }
        let url = try Self.normalize(trimmed)
        defaults.set(url.absoluteString, forKey: Self.addressKey)
    }

    /// Validates and canonicalizes one address.
    ///
    /// A trailing slash is removed because every path this side sends is
    /// absolute, and `https://host//v1/...` is a different route to a strict
    /// router.
    static func normalize(_ raw: String) throws -> URL {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        var stripped = candidate
        while stripped.hasSuffix("/") { stripped.removeLast() }
        guard
            let url = URL(string: stripped),
            let scheme = url.scheme?.lowercased(),
            scheme == "https" || scheme == "http",
            let host = url.host,
            !host.isEmpty
        else {
            throw ControlPlaneHostError.invalidAddress(trimmed)
        }
        return url
    }

    /// Ensures this Mac is an enrolled host device and returns its credentials.
    ///
    /// Enrollment is created on demand rather than at launch: a Mac that never
    /// pairs a phone never registers anything, and turning remote access on is
    /// not by itself a decision to appear in a cloud directory.
    func enrollment(publicKey: String, name: String) async throws -> HostEnrollment {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let api = apiFactory(address)

        if let existing = try store.load(), existing.address == address.absoluteString {
            guard existing.publicKey != publicKey else { return existing }
            // The Mac identity was rotated locally. Rotation is preferred over
            // re-enrollment because it keeps this Mac's pairings intact.
            try await api.rotateHostKey(
                deviceToken: existing.deviceToken,
                deviceID: existing.deviceID,
                publicKey: publicKey
            )
            let rotated = HostEnrollment(
                address: existing.address,
                accountToken: existing.accountToken,
                deviceID: existing.deviceID,
                deviceToken: existing.deviceToken,
                publicKey: publicKey
            )
            try store.save(rotated)
            return rotated
        }

        let accountToken = try await api.createAccount(label: name)
        let host = try await api.enrollHost(
            accountToken: accountToken,
            name: name,
            publicKey: publicKey
        )
        let credentials = HostEnrollment(
            address: address.absoluteString,
            accountToken: accountToken,
            deviceID: host.deviceID,
            deviceToken: host.deviceToken,
            publicKey: publicKey
        )
        try store.save(credentials)
        return credentials
    }

    /// Registers a displayed pairing code and returns the material with the
    /// address the phone should enroll against attached.
    func openPairing(
        _ material: PairingMaterial,
        publicKey: String,
        macName: String,
        permission: DevicePermission = .interact
    ) async throws -> PairingMaterial {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let credentials = try await enrollment(publicKey: publicKey, name: macName)
        try await apiFactory(address).openPairingRequest(
            deviceToken: credentials.deviceToken,
            pairingID: material.pairingID,
            secretDigest: Self.secretDigest(material.secret),
            permission: permission
        )
        return material.addressed(to: address, macName: macName)
    }

    /// The phones the control plane says are paired with this Mac.
    func pairedClients() async throws -> [ControlPlaneDevice] {
        guard let address, let credentials = try store.load(),
              credentials.address == address.absoluteString else {
            throw ControlPlaneHostError.notConfigured
        }
        return try await apiFactory(address)
            .devices(deviceToken: credentials.deviceToken)
            .filter { $0.role == "client" && !$0.revoked }
    }

    /// Forgets this Mac's control-plane credentials without touching the
    /// address, so the next pairing enrolls afresh.
    func forgetEnrollment() throws {
        try store.clear()
    }

    /// The digest the control plane stores in place of the pairing secret.
    ///
    /// It is domain-separated exactly as `credentials.ts` does it: the same
    /// string hashed for a different purpose must not collide with this one.
    static func secretDigest(_ secret: String) -> String {
        let digest = SHA256.hash(data: Data("latch/v1/pairing \(secret)".utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }
}
