import CryptoKit
import Foundation
import Security

/// Credentials for this Mac at one control-plane deployment.
struct HostEnrollment: Codable, Equatable, Sendable {
    let address: String
    let accountToken: String
    let deviceID: String
    let deviceToken: String
    let publicKey: String
}

protocol HostEnrollmentStoring: Sendable {
    func load() throws -> HostEnrollment?
    func save(_ enrollment: HostEnrollment) throws
    func clear() throws
}

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
        do {
            return try JSONDecoder().decode(HostEnrollment.self, from: data)
        } catch {
            throw ControlPlaneHostError.storage("this Mac's stored control-plane credentials are malformed")
        }
    }

    func save(_ enrollment: HostEnrollment) throws {
        let data = try JSONEncoder().encode(enrollment)
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

final class MemoryHostEnrollmentStore: HostEnrollmentStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var enrollment: HostEnrollment?

    init(_ enrollment: HostEnrollment? = nil) { self.enrollment = enrollment }

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

enum ControlPlaneHostError: LocalizedError, Equatable, Sendable {
    case notConfigured
    case invalidAddress(String)
    case noIdentity
    case http(status: Int, path: String, reason: String)
    case malformedResponse(String)
    case transport(String)
    case storage(String)

    var errorDescription: String? {
        switch self {
        case .notConfigured:
            return "No control plane is set for this Mac. Add its HTTPS address in Remote Access settings."
        case .invalidAddress(let value):
            return "\(value) is not a control-plane address. Use an https:// address."
        case .noIdentity:
            return "This Mac has no remote-access identity yet. Turn remote access on first."
        case .http(let status, let path, let reason):
            return "The control plane refused \(path) (\(status)): \(reason)"
        case .malformedResponse(let detail):
            return "The control plane sent an unexpected response: \(detail)"
        case .transport(let detail), .storage(let detail):
            return detail
        }
    }

    var isStaleCredential: Bool {
        guard case .http(let status, _, _) = self else { return false }
        return status == 401 || status == 404
    }
}

enum ControlPlaneLabel {
    static let fallback = "Latch Mac"
    private static let maxScalars = 64
    private static let substitutions: [Character: Character] = [
        "\u{2018}": "'", "\u{2019}": "'", "\u{02BC}": "'", "\u{00B4}": "'", "`": "'",
        "\u{2013}": "-", "\u{2014}": "-", "\u{2212}": "-",
    ]
    private static let allowed: CharacterSet = CharacterSet.letters
        .union(.decimalDigits)
        .union(CharacterSet(charactersIn: " ._'()-"))
        .subtracting(.nonBaseCharacters)

    static func enrollable(_ raw: String, fallback: String = ControlPlaneLabel.fallback) -> String {
        let composed = raw.precomposedStringWithCanonicalMapping
        let folded = String(composed.map { substitutions[$0] ?? $0 })
        let cleaned = String(
            String.UnicodeScalarView(folded.unicodeScalars.map { allowed.contains($0) ? $0 : " " })
        )
        var bounded = cleaned.split(separator: " ", omittingEmptySubsequences: true).joined(separator: " ")
        while bounded.unicodeScalars.count > maxScalars { bounded.removeLast() }
        let trimmed = bounded.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? fallback : trimmed
    }
}

/// A helper launch plus the directory identity of the phone it serves.
struct RemoteLinkAssignment: Equatable, Sendable {
    let peerDeviceID: String
    let configuration: RemoteLinkHostConfiguration
}

/// Relay URL and single-use admission for one WSS socket.
struct RemoteLinkAdmission: Equatable, Sendable {
    let relayURL: String
    let admission: String
}

struct ControlPlaneRemoteLink: Decodable, Equatable, Sendable {
    let version: Int
    let linkId: String
    let peerDeviceId: String
    let peerPublicKey: String
    let permission: DevicePermission
    let grantRevision: UInt64
}

struct ControlPlaneRelayAdmission: Decodable, Equatable, Sendable {
    let version: Int
    let relayUrl: String
    let admission: String
    let expiresAt: UInt64
}

struct ControlPlaneLeaseExtension: Decodable, Equatable, Sendable {
    let leaseId: String
    let expiresAt: UInt64
    let claim: String

    enum CodingKeys: String, CodingKey {
        case leaseId, expiresAt
        case claim = "extension"
    }
}

struct ControlPlaneEnrollment: Decodable, Equatable, Sendable {
    let version: Int
    let enrollmentId: String
    let expiresAt: UInt64
    let relayUrl: String
    let hostPublicKey: String
    let admissionCode: String
    let hostAdmission: String
}

struct ControlPlaneEnrollmentReceipt: Decodable, Equatable, Sendable {
    let version: Int
    let enrollmentId: String
    let hostPublicKey: String
    let controllerPublicKey: String
    let permission: DevicePermission
    let grantRevision: UInt64
    let remoteLinkId: String
}

protocol ControlPlaneHostAPI: Sendable {
    func claimAccount(invitation: String, label: String) async throws -> String
    func enrollHost(accountToken: String, name: String, publicKey: String) async throws -> (deviceID: String, deviceToken: String)
    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws
    func openEnrollment(deviceToken: String) async throws -> ControlPlaneEnrollment
    func completeEnrollment(
        deviceToken: String,
        enrollmentID: String,
        controllerPublicKey: String,
        permission: DevicePermission,
        grantRevision: UInt64
    ) async throws -> ControlPlaneEnrollmentReceipt
    func cancelEnrollment(deviceToken: String, enrollmentID: String) async throws
    func setPairingPermission(deviceToken: String, clientDeviceID: String, permission: DevicePermission) async throws
    func revokePairing(deviceToken: String, clientDeviceID: String) async throws
    func remoteLinks(deviceToken: String) async throws -> [ControlPlaneRemoteLink]
    func relayAdmission(deviceToken: String, peerDeviceID: String) async throws -> ControlPlaneRelayAdmission
    func renewRelayLease(deviceToken: String, leaseID: String) async throws -> ControlPlaneLeaseExtension
    func setRelayEnabled(accountToken: String, enabled: Bool) async throws
    /// Asks for one generic attention alert. Returns whether it was delivered.
    func notifyAttention(deviceToken: String, clientDeviceID: String, eventID: String) async throws -> Bool
}

actor HTTPControlPlaneHostAPI: ControlPlaneHostAPI {
    private let baseURL: URL
    private let session: URLSession

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func claimAccount(invitation: String, label: String) async throws -> String {
        struct Response: Decodable { let accountToken: String }
        let response: Response = try await send(
            path: "/v1/accounts/claim", method: "POST", token: nil,
            body: ["invitation": invitation, "label": label]
        )
        return response.accountToken
    }

    func enrollHost(accountToken: String, name: String, publicKey: String) async throws -> (deviceID: String, deviceToken: String) {
        struct Response: Decodable { let deviceId: String; let deviceToken: String }
        let response: Response = try await send(
            path: "/v1/devices", method: "POST", token: accountToken,
            body: ["name": name, "platform": "macos", "role": "host", "publicKey": publicKey]
        )
        return (response.deviceId, response.deviceToken)
    }

    func rotateHostKey(deviceToken: String, deviceID: String, publicKey: String) async throws {
        let _: Empty = try await send(
            path: "/v1/devices/\(escape(deviceID))/rotate-key", method: "POST",
            token: deviceToken, body: ["publicKey": publicKey]
        )
    }

    func openEnrollment(deviceToken: String) async throws -> ControlPlaneEnrollment {
        try await send(path: "/v1/enrollments", method: "POST", token: deviceToken, body: [:])
    }

    func completeEnrollment(
        deviceToken: String,
        enrollmentID: String,
        controllerPublicKey: String,
        permission: DevicePermission,
        grantRevision: UInt64
    ) async throws -> ControlPlaneEnrollmentReceipt {
        try await send(
            path: "/v1/enrollments/\(escape(enrollmentID))/complete", method: "POST", token: deviceToken,
            body: [
                "controllerPublicKey": controllerPublicKey,
                "permission": permission.rawValue,
                "grantRevision": grantRevision,
            ]
        )
    }

    func cancelEnrollment(deviceToken: String, enrollmentID: String) async throws {
        let _: Empty = try await send(
            path: "/v1/enrollments/\(escape(enrollmentID))", method: "DELETE", token: deviceToken, body: nil
        )
    }

    func setPairingPermission(deviceToken: String, clientDeviceID: String, permission: DevicePermission) async throws {
        let _: Empty = try await send(
            path: "/v1/pairings", method: "POST", token: deviceToken,
            body: ["clientDeviceId": clientDeviceID, "permission": permission.rawValue]
        )
    }

    func revokePairing(deviceToken: String, clientDeviceID: String) async throws {
        let _: Empty = try await send(
            path: "/v1/pairings/\(escape(clientDeviceID))", method: "DELETE", token: deviceToken, body: nil
        )
    }

    func remoteLinks(deviceToken: String) async throws -> [ControlPlaneRemoteLink] {
        struct Response: Decodable { let links: [ControlPlaneRemoteLink] }
        let response: Response = try await send(path: "/v1/remote-links", method: "GET", token: deviceToken, body: nil)
        return response.links
    }

    func relayAdmission(deviceToken: String, peerDeviceID: String) async throws -> ControlPlaneRelayAdmission {
        try await send(
            path: "/v1/relay-admissions", method: "POST", token: deviceToken,
            body: ["peerDeviceId": peerDeviceID]
        )
    }

    func renewRelayLease(deviceToken: String, leaseID: String) async throws -> ControlPlaneLeaseExtension {
        try await send(
            path: "/v1/relay-leases/\(escape(leaseID))/renew", method: "POST", token: deviceToken, body: [:]
        )
    }

    func setRelayEnabled(accountToken: String, enabled: Bool) async throws {
        let _: Empty = try await send(
            path: "/v1/account", method: "PATCH", token: accountToken, body: ["relayEnabled": enabled]
        )
    }

    func notifyAttention(deviceToken: String, clientDeviceID: String, eventID: String) async throws -> Bool {
        struct Response: Decodable { let delivered: Bool }
        let response: Response = try await send(
            path: "/v1/attention", method: "POST", token: deviceToken,
            body: ["clientDeviceId": clientDeviceID, "eventId": eventID]
        )
        return response.delivered
    }

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
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
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
        if data.isEmpty, let empty = Empty() as? Response { return empty }
        do {
            return try JSONDecoder().decode(Response.self, from: data)
        } catch {
            throw ControlPlaneHostError.malformedResponse(String(describing: error))
        }
    }
}

@MainActor
final class ControlPlaneHost {
    static let addressKey = "remoteAccessControlPlane"

    private let store: HostEnrollmentStoring
    private let defaults: UserDefaults
    private let apiFactory: @Sendable (URL) -> ControlPlaneHostAPI
    private var ownerInvitation: String?

    init(
        store: HostEnrollmentStoring = KeychainHostEnrollmentStore(),
        defaults: UserDefaults = LatchClient.preferences,
        apiFactory: @escaping @Sendable (URL) -> ControlPlaneHostAPI = { HTTPControlPlaneHostAPI(baseURL: $0) }
    ) {
        self.store = store
        self.defaults = defaults
        self.apiFactory = apiFactory
    }

    var address: URL? {
        guard let raw = defaults.string(forKey: Self.addressKey), !raw.isEmpty else { return nil }
        return try? Self.normalize(raw)
    }
    var isConfigured: Bool { address != nil }

    func setOwnerInvitation(_ raw: String) throws {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { ownerInvitation = nil; return }
        guard trimmed.range(of: #"^inv_[0-9a-f]{32}\.[0-9a-f]{64}$"#, options: .regularExpression) != nil else {
            throw ControlPlaneHostError.malformedResponse("owner invitation has an invalid format")
        }
        ownerInvitation = trimmed
    }

    func setAddress(_ raw: String) throws {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { defaults.removeObject(forKey: Self.addressKey); return }
        defaults.set(try Self.normalize(trimmed).absoluteString, forKey: Self.addressKey)
    }

    static func normalize(_ raw: String) throws -> URL {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        var stripped = candidate
        while stripped.hasSuffix("/") { stripped.removeLast() }
        guard let url = URL(string: stripped),
              ["https", "http"].contains(url.scheme?.lowercased() ?? ""),
              let host = url.host, !host.isEmpty else {
            throw ControlPlaneHostError.invalidAddress(trimmed)
        }
        return url
    }

    func enrollment(publicKey: String, name: String) async throws -> HostEnrollment {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let api = apiFactory(address)
        if let existing = try store.load(), existing.address == address.absoluteString {
            guard existing.publicKey != publicKey else { return existing }
            do {
                try await api.rotateHostKey(
                    deviceToken: existing.deviceToken, deviceID: existing.deviceID, publicKey: publicKey
                )
            } catch let error as ControlPlaneHostError where error.isStaleCredential {
                try store.clear()
                return try await freshEnrollment(api: api, address: address, publicKey: publicKey, name: name)
            }
            let rotated = HostEnrollment(
                address: existing.address, accountToken: existing.accountToken,
                deviceID: existing.deviceID, deviceToken: existing.deviceToken, publicKey: publicKey
            )
            try store.save(rotated)
            return rotated
        }
        return try await freshEnrollment(api: api, address: address, publicKey: publicKey, name: name)
    }

    private func freshEnrollment(
        api: ControlPlaneHostAPI,
        address: URL,
        publicKey: String,
        name: String
    ) async throws -> HostEnrollment {
        let label = ControlPlaneLabel.enrollable(name)
        guard let invitation = ownerInvitation else {
            throw ControlPlaneHostError.malformedResponse(
                "this Mac needs a one-use owner invitation before its first control-plane enrollment"
            )
        }
        ownerInvitation = nil
        let accountToken = try await api.claimAccount(invitation: invitation, label: label)
        let host = try await api.enrollHost(accountToken: accountToken, name: label, publicKey: publicKey)
        let result = HostEnrollment(
            address: address.absoluteString, accountToken: accountToken,
            deviceID: host.deviceID, deviceToken: host.deviceToken, publicKey: publicKey
        )
        try store.save(result)
        return result
    }

    func openRemoteEnrollment(publicKey: String, macName: String) async throws -> RemoteEnrollmentMaterial {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let api = apiFactory(address)
        var credentials = try await enrollment(publicKey: publicKey, name: macName)
        let opened: ControlPlaneEnrollment
        do {
            opened = try await api.openEnrollment(deviceToken: credentials.deviceToken)
        } catch let error as ControlPlaneHostError where error.isStaleCredential {
            try store.clear()
            credentials = try await enrollment(publicKey: publicKey, name: macName)
            opened = try await api.openEnrollment(deviceToken: credentials.deviceToken)
        }
        guard opened.version == 1, opened.hostPublicKey == publicKey,
              opened.relayUrl.hasPrefix("wss://") else {
            throw ControlPlaneHostError.malformedResponse("enrollment did not preserve this Mac's exact key and secure relay")
        }
        let secret = SymmetricKey(size: .bits256).withUnsafeBytes {
            $0.map { String(format: "%02x", $0) }.joined()
        }
        return RemoteEnrollmentMaterial(
            enrollmentID: opened.enrollmentId, enrollmentSecret: secret,
            hostPublicKey: opened.hostPublicKey, admissionCode: opened.admissionCode,
            hostAdmission: opened.hostAdmission, relayURL: opened.relayUrl,
            expiresAt: opened.expiresAt, controlPlane: address.absoluteString, macName: macName
        )
    }

    func completeRemoteEnrollment(
        enrollmentID: String,
        controllerPublicKey: String,
        permission: DevicePermission,
        grantRevision: UInt64
    ) async throws -> ControlPlaneEnrollmentReceipt {
        let (address, credentials) = try configuredCredentials()
        let receipt = try await apiFactory(address).completeEnrollment(
            deviceToken: credentials.deviceToken, enrollmentID: enrollmentID,
            controllerPublicKey: controllerPublicKey, permission: permission, grantRevision: grantRevision
        )
        guard receipt.version == 1, receipt.enrollmentId == enrollmentID,
              receipt.hostPublicKey == credentials.publicKey,
              receipt.controllerPublicKey == controllerPublicKey,
              receipt.permission == permission,
              receipt.grantRevision == grantRevision else {
            throw ControlPlaneHostError.malformedResponse("enrollment receipt did not match the locally approved key and grant")
        }
        return receipt
    }

    func cancelRemoteEnrollment(_ enrollmentID: String) async {
        guard let address, let credentials = try? store.load(),
              credentials.address == address.absoluteString else { return }
        try? await apiFactory(address).cancelEnrollment(
            deviceToken: credentials.deviceToken, enrollmentID: enrollmentID
        )
    }

    func mirrorPermission(clientDeviceID: String, permission: DevicePermission) async throws {
        let (address, credentials) = try configuredCredentials()
        try await apiFactory(address).setPairingPermission(
            deviceToken: credentials.deviceToken, clientDeviceID: clientDeviceID, permission: permission
        )
    }

    func revokePairing(clientDeviceID: String) async throws {
        let (address, credentials) = try configuredCredentials()
        try await apiFactory(address).revokePairing(
            deviceToken: credentials.deviceToken, clientDeviceID: clientDeviceID
        )
    }

    func remoteLinkConfigurations(publicKey: String, macName: String) async throws -> [RemoteLinkHostConfiguration] {
        try await remoteLinkAssignments(publicKey: publicKey, macName: macName).map(\.configuration)
    }

    /// One helper launch per current pairing, with the peer's directory id so
    /// the helper can be re-admitted after its relay socket closes.
    func remoteLinkAssignments(publicKey: String, macName: String) async throws -> [RemoteLinkAssignment] {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let credentials = try await enrollment(publicKey: publicKey, name: macName)
        let api = apiFactory(address)
        let links = try await api.remoteLinks(deviceToken: credentials.deviceToken)
        var assignments: [RemoteLinkAssignment] = []
        for link in links where link.version == 1 && link.grantRevision > 0 {
            let admission = try await api.relayAdmission(
                deviceToken: credentials.deviceToken, peerDeviceID: link.peerDeviceId
            )
            guard admission.version == 1, admission.relayUrl.hasPrefix("wss://") else {
                throw ControlPlaneHostError.malformedResponse("unsupported or insecure Remote Link admission")
            }
            assignments.append(RemoteLinkAssignment(
                peerDeviceID: link.peerDeviceId,
                configuration: RemoteLinkHostConfiguration(
                    purpose: "session", relayUrl: admission.relayUrl, admission: admission.admission,
                    peerPublicKey: link.peerPublicKey, grantRevision: link.grantRevision
                )
            ))
        }
        return assignments
    }

    /// A fresh single-use admission for a helper whose relay socket ended.
    func freshRelayAdmission(peerDeviceID: String) async throws -> RemoteLinkAdmission {
        let (address, credentials) = try configuredCredentials()
        let admission = try await apiFactory(address).relayAdmission(
            deviceToken: credentials.deviceToken, peerDeviceID: peerDeviceID
        )
        guard admission.version == 1, admission.relayUrl.hasPrefix("wss://") else {
            throw ControlPlaneHostError.malformedResponse("unsupported or insecure Remote Link admission")
        }
        return RemoteLinkAdmission(relayURL: admission.relayUrl, admission: admission.admission)
    }

    func notifyAttention(clientDeviceID: String, eventID: String) async throws -> Bool {
        let (address, credentials) = try configuredCredentials()
        return try await apiFactory(address).notifyAttention(
            deviceToken: credentials.deviceToken, clientDeviceID: clientDeviceID, eventID: eventID
        )
    }

    func renewRemoteLinkLease(_ leaseID: String) async throws -> ControlPlaneLeaseExtension {
        let (address, credentials) = try configuredCredentials()
        return try await apiFactory(address).renewRelayLease(
            deviceToken: credentials.deviceToken, leaseID: leaseID
        )
    }

    func setRelayEnabled(_ enabled: Bool) async throws {
        let (address, credentials) = try configuredCredentials()
        try await apiFactory(address).setRelayEnabled(accountToken: credentials.accountToken, enabled: enabled)
    }

    func forgetEnrollment() throws { try store.clear() }

    private func configuredCredentials() throws -> (URL, HostEnrollment) {
        guard let address, let credentials = try store.load(),
              credentials.address == address.absoluteString else {
            throw ControlPlaneHostError.notConfigured
        }
        return (address, credentials)
    }
}
