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

    /// Whether the answer means the credentials this Mac is holding no longer
    /// name anything the control plane knows.
    ///
    /// A redeployed or reset service issues no new token to the Mac that was
    /// enrolled against the old one, so the Keychain keeps a credential that
    /// can only ever be refused. That is indistinguishable from a permanently
    /// broken control plane unless it is recognized and enrolled afresh.
    var isStaleCredential: Bool {
        guard case .http(let status, _, _) = self else { return false }
        return status == 401 || status == 404
    }
}

// MARK: - Names the control plane will store

/// Reduces a name to the label set the control plane accepts.
///
/// The service takes letters, digits, spaces, and `. _ ' ( ) -`, up to 64
/// characters, and answers anything else with a 400. macOS names a Mac
/// "Jake’s MacBook Pro" using the typographic apostrophe, so the default
/// name of an ordinary Mac is refused on the very first call, and the pairing
/// fails with a message about the control plane rather than about the name.
///
/// Typographic punctuation is folded to its ASCII equivalent before anything is
/// dropped, so a name survives as a name: "Jake’s MacBook Pro" enrolls as
/// "Jake's MacBook Pro" rather than losing the character to a space. The name is
/// composed first for the same reason: the service matches letters, and a
/// decomposed accent is a combining mark rather than part of one.
enum ControlPlaneLabel {
    /// What a Mac whose name survives as nothing is called instead.
    static let fallback = "Latch Mac"

    /// The service counts code points, so the bound is counted the same way
    /// rather than in grapheme clusters, which can be several code points each.
    private static let maxScalars = 64

    private static let substitutions: [Character: Character] = [
        "\u{2018}": "'", "\u{2019}": "'", "\u{02BC}": "'", "\u{00B4}": "'", "`": "'",
        "\u{2013}": "-", "\u{2014}": "-", "\u{2212}": "-",
    ]

    /// `CharacterSet.letters` includes combining marks, so a variation
    /// selector or a stray accent would otherwise survive as an invisible
    /// "letter" the service then rejects — a Mac named "Mac 🖥️ Studio" reduces
    /// to "Mac ️ Studio", with the emoji gone and its selector left behind.
    /// Composition above has already folded every mark that belongs to a base
    /// character, so whatever is still non-base here has nothing to attach to.
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
        var bounded = cleaned
            .split(separator: " ", omittingEmptySubsequences: true)
            .joined(separator: " ")
        while bounded.unicodeScalars.count > maxScalars {
            bounded.removeLast()
        }
        let trimmed = bounded.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? fallback : trimmed
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

/// A short-lived transport address advertised to a paired device.
///
/// `address` is still only an IP literal and port, and the ICE metadata around
/// it is structured rather than an opaque SDP blob for exactly that reason: the
/// address is the privacy control, and it stays inspectable. Nothing here is
/// the loopback gateway address or its per-launch credential.
///
/// The ICE fields are optional as a set. A publisher with no ICE agent yet
/// omits all of them, which the service accepts; a publisher that supplies any
/// of them must supply all five, which `validatedForPublication()` enforces
/// before the request leaves this Mac.
struct ControlPlaneCandidate: Codable, Equatable, Sendable {
    let address: String
    let expiresAt: UInt64
    /// `host`, `srflx`, `prflx`, or `relay`.
    let type: String?
    let priority: UInt32?
    let foundation: String?
    /// 1 (RTP) or 2 (RTCP). Latch uses a single component.
    let component: Int?
    let `protocol`: String?
    let relatedAddress: String?
    let relatedPort: Int?
    /// Only meaningful for TCP candidates: `active`, `passive`, or `so`.
    let tcpType: String?

    init(
        address: String,
        expiresAt: UInt64,
        type: String? = nil,
        priority: UInt32? = nil,
        foundation: String? = nil,
        component: Int? = nil,
        protocol proto: String? = nil,
        relatedAddress: String? = nil,
        relatedPort: Int? = nil,
        tcpType: String? = nil
    ) {
        self.address = address
        self.expiresAt = expiresAt
        self.type = type
        self.priority = priority
        self.foundation = foundation
        self.component = component
        self.protocol = proto
        self.relatedAddress = relatedAddress
        self.relatedPort = relatedPort
        self.tcpType = tcpType
    }

    private static let types = ["host", "srflx", "prflx", "relay"]
    private static let protocols = ["udp", "tcp"]
    private static let tcpTypes = ["active", "passive", "so"]

    /// Applies the service's candidate rules locally so a malformed candidate
    /// is refused here rather than costing a round trip and a 400. These are
    /// the same all-or-nothing metadata, paired related-address, and
    /// TCP-only-`tcpType` rules `validation.ts` enforces.
    func validatedForPublication() throws -> ControlPlaneCandidate {
        let required: [Bool] = [
            type != nil, priority != nil, foundation != nil, component != nil, self.protocol != nil,
        ]
        if required.contains(true), required.contains(false) {
            throw ControlPlaneHostError.malformedResponse(
                "an ICE candidate must carry type, priority, foundation, component, and protocol together"
            )
        }
        if let type, !Self.types.contains(type) {
            throw ControlPlaneHostError.malformedResponse("\(type) is not an ICE candidate type")
        }
        if let proto = self.protocol, !Self.protocols.contains(proto) {
            throw ControlPlaneHostError.malformedResponse("\(proto) is not an ICE transport")
        }
        if let component, component != 1, component != 2 {
            throw ControlPlaneHostError.malformedResponse("an ICE component is 1 or 2")
        }
        if let foundation, foundation.isEmpty || foundation.count > 32 {
            throw ControlPlaneHostError.malformedResponse("an ICE foundation is 1 to 32 characters")
        }
        if (relatedAddress == nil) != (relatedPort == nil) {
            throw ControlPlaneHostError.malformedResponse(
                "relatedAddress and relatedPort are published together or not at all"
            )
        }
        if let relatedAddress, ControlPlaneHost.isLoopbackHost(relatedAddress) {
            throw ControlPlaneHostError.malformedResponse("a related address must not be loopback")
        }
        if let tcpType {
            guard Self.tcpTypes.contains(tcpType) else {
                throw ControlPlaneHostError.malformedResponse("\(tcpType) is not a TCP candidate type")
            }
            guard self.protocol == "tcp" else {
                throw ControlPlaneHostError.malformedResponse("tcpType applies only to TCP candidates")
            }
        }
        return self
    }

    /// The wire form, with absent fields omitted rather than sent as null: the
    /// service rejects unknown and malformed properties, and a null is neither
    /// an integer nor a string.
    var requestBody: [String: Any] {
        var body: [String: Any] = ["address": address, "expiresAt": expiresAt]
        if let type { body["type"] = type }
        if let priority { body["priority"] = priority }
        if let foundation { body["foundation"] = foundation }
        if let component { body["component"] = component }
        if let proto = self.protocol { body["protocol"] = proto }
        if let relatedAddress { body["relatedAddress"] = relatedAddress }
        if let relatedPort { body["relatedPort"] = relatedPort }
        if let tcpType { body["tcpType"] = tcpType }
        return body
    }
}

/// The short-term ICE credentials connectivity checks authenticate with.
///
/// These are scoped to the lifetime of the agent that will use them — the
/// helper process — not to a presence refresh. Rotating them on every refresh
/// would race a phone that read them at the start of a presence window and
/// began its checks at the end of it: its binding requests would carry a ufrag
/// the Mac no longer recognises and every check would fail with 401.
struct ControlPlaneIceCredentials: Equatable, Sendable {
    let ufrag: String
    let pwd: String

    /// RFC 8445 requires at least 24 bits of ufrag and 128 bits of password;
    /// these are 48 and 144, inside the service's length bounds.
    static func generate() -> ControlPlaneIceCredentials {
        ControlPlaneIceCredentials(ufrag: randomToken(bytes: 6), pwd: randomToken(bytes: 18))
    }

    private static func randomToken(bytes count: Int) -> String {
        var generator = SystemRandomNumberGenerator()
        let bytes = (0..<count).map { _ in UInt8.random(in: 0...255, using: &generator) }
        // base64url: inside the service's `[A-Za-z0-9+/_=-]` alphabet, and free
        // of characters that would need escaping in an SDP attribute line.
        return Data(bytes).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// An offer waiting for this Mac. It is deliberately transport-only: session
/// data and gateway credentials never cross the control plane.
struct ControlPlaneRendezvousOffer: Decodable, Equatable, Sendable {
    let requestID: String
    let peerDeviceID: String
    let peerIdentityKey: String
    let candidates: [ControlPlaneCandidate]
    /// The offering peer's ICE credentials. Absent when that peer published no
    /// agent of its own, which is why they are optional rather than required.
    let iceUfrag: String?
    let icePwd: String?
    let expiresAt: UInt64

    init(
        requestID: String,
        peerDeviceID: String,
        peerIdentityKey: String,
        candidates: [ControlPlaneCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        expiresAt: UInt64
    ) {
        self.requestID = requestID
        self.peerDeviceID = peerDeviceID
        self.peerIdentityKey = peerIdentityKey
        self.candidates = candidates
        self.iceUfrag = iceUfrag
        self.icePwd = icePwd
        self.expiresAt = expiresAt
    }

    enum CodingKeys: String, CodingKey {
        case candidates, expiresAt, iceUfrag, icePwd
        case requestID = "requestId"
        case peerDeviceID = "peerDeviceId"
        case peerIdentityKey
    }
}

/// The control-plane's bounded presence window. The service is authoritative
/// if this ever changes; the client uses the returned value to schedule its
/// next refresh before expiry.
struct ControlPlanePresence: Decodable, Equatable, Sendable {
    let expiresAt: UInt64
    let ttlSeconds: UInt64
}

/// One entry of the control plane's ICE server list, in the WebRTC
/// `RTCIceServer` shape it returns from `GET /v1/ice-servers` and
/// `POST /v1/turn-credentials`. Only the STUN entries reach the helper: TURN
/// allocation is the phone's decision, made under its own credentials.
struct ControlPlaneIceServer: Decodable, Equatable, Sendable {
    let urls: [String]
    let username: String?
    let credential: String?

    init(urls: [String], username: String? = nil, credential: String? = nil) {
        self.urls = urls
        self.username = username
        self.credential = credential
    }

    /// Whether a URL names a STUN server rather than a relay.
    static func isStun(_ url: String) -> Bool {
        let scheme = url.split(separator: ":", maxSplits: 1).first.map(String.init)?.lowercased()
        return scheme == "stun" || scheme == "stuns"
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
    func setPairingPermission(
        deviceToken: String,
        clientDeviceID: String,
        permission: DevicePermission
    ) async throws
    func devices(deviceToken: String) async throws -> [ControlPlaneDevice]
    func publishPresence(
        deviceToken: String,
        candidates: [ControlPlaneCandidate],
        ice: ControlPlaneIceCredentials?
    ) async throws -> ControlPlanePresence
    func clearPresence(deviceToken: String) async throws
    /// Collects offers, holding the request for up to `wait` seconds when
    /// none is queued. Zero answers immediately.
    func rendezvousOffers(deviceToken: String, wait: UInt64) async throws -> [ControlPlaneRendezvousOffer]
    /// The STUN servers a device may gather reflexive candidates against.
    func iceServers(deviceToken: String) async throws -> [ControlPlaneIceServer]
    func setRelayEnabled(accountToken: String, enabled: Bool) async throws
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

    /// Restates a pairing this Mac already authorized locally, at its current
    /// permission. `POST /v1/pairings` is an upsert keyed by the pair, so this
    /// is a permission update rather than a second pairing.
    func setPairingPermission(
        deviceToken: String,
        clientDeviceID: String,
        permission: DevicePermission
    ) async throws {
        let _: Empty = try await send(
            path: "/v1/pairings",
            method: "POST",
            token: deviceToken,
            body: [
                "clientDeviceId": clientDeviceID,
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

    func publishPresence(
        deviceToken: String,
        candidates: [ControlPlaneCandidate],
        ice: ControlPlaneIceCredentials?
    ) async throws -> ControlPlanePresence {
        // The credentials are sent as a pair or not at all: the service refuses
        // one without the other, and a lone ufrag authenticates nothing.
        var body: [String: Any] = ["candidates": candidates.map(\.requestBody)]
        if let ice {
            body["iceUfrag"] = ice.ufrag
            body["icePwd"] = ice.pwd
        }
        return try await send(
            path: "/v1/presence",
            method: "POST",
            token: deviceToken,
            body: body
        )
    }

    func clearPresence(deviceToken: String) async throws {
        let _: Empty = try await send(
            path: "/v1/presence",
            method: "DELETE",
            token: deviceToken,
            body: nil
        )
    }

    func rendezvousOffers(deviceToken: String, wait: UInt64) async throws -> [ControlPlaneRendezvousOffer] {
        struct Response: Decodable { let offers: [ControlPlaneRendezvousOffer] }
        // Annotated rather than inferred through the member access: the
        // inference chain through the generic `send` is what the type checker
        // gives up on here.
        let response: Response = try await send(
            path: wait > 0 ? "/v1/rendezvous?wait=\(wait)" : "/v1/rendezvous",
            method: "GET",
            token: deviceToken,
            body: nil
        )
        return response.offers
    }

    func iceServers(deviceToken: String) async throws -> [ControlPlaneIceServer] {
        struct Response: Decodable { let iceServers: [ControlPlaneIceServer] }
        let response: Response = try await send(
            path: "/v1/ice-servers",
            method: "GET",
            token: deviceToken,
            body: nil
        )
        return response.iceServers
    }

    func setRelayEnabled(accountToken: String, enabled: Bool) async throws {
        let _: Empty = try await send(
            path: "/v1/account",
            method: "PATCH",
            token: accountToken,
            body: ["relayEnabled": enabled]
        )
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
            do {
                try await api.rotateHostKey(
                    deviceToken: existing.deviceToken,
                    deviceID: existing.deviceID,
                    publicKey: publicKey
                )
            } catch let error as ControlPlaneHostError where error.isStaleCredential {
                // There is nothing left to rotate: this Mac's device row is
                // gone from the control plane, so it enrolls again instead of
                // failing every future pairing with a refusal the person
                // cannot clear from the UI.
                try store.clear()
                return try await freshEnrollment(api: api, address: address, publicKey: publicKey, name: name)
            }
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

        return try await freshEnrollment(api: api, address: address, publicKey: publicKey, name: name)
    }

    /// Creates the account and host device this Mac will use from now on.
    ///
    /// The name is reduced to the label set the service stores before it is
    /// sent. A Mac carries whatever name its owner gave it, and a name the
    /// control plane refuses would otherwise stop pairing before the phone is
    /// ever involved.
    private func freshEnrollment(
        api: ControlPlaneHostAPI,
        address: URL,
        publicKey: String,
        name: String
    ) async throws -> HostEnrollment {
        let label = ControlPlaneLabel.enrollable(name)
        let accountToken = try await api.createAccount(label: label)
        let host = try await api.enrollHost(
            accountToken: accountToken,
            name: label,
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
        permission: DevicePermission = .control
    ) async throws -> PairingMaterial {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let api = apiFactory(address)
        let credentials = try await enrollment(publicKey: publicKey, name: macName)
        do {
            try await register(material, with: credentials, on: api, permission: permission)
        } catch let error as ControlPlaneHostError where error.isStaleCredential {
            // The stored device credential names nothing the control plane
            // still has — a redeployed or reset service, most often. Enrolling
            // once more is the only way back, and it restores no authorization
            // on its own: the new host device holds no pairings until a phone
            // scans a code for it.
            try store.clear()
            let renewed = try await enrollment(publicKey: publicKey, name: macName)
            try await register(material, with: renewed, on: api, permission: permission)
        }
        return material.addressed(to: address, macName: macName)
    }

    private func register(
        _ material: PairingMaterial,
        with credentials: HostEnrollment,
        on api: ControlPlaneHostAPI,
        permission: DevicePermission
    ) async throws {
        try await api.openPairingRequest(
            deviceToken: credentials.deviceToken,
            pairingID: material.pairingID,
            secretDigest: Self.secretDigest(material.secret),
            permission: permission
        )
    }

    /// Mirrors a local grant change into the control-plane directory.
    ///
    /// The Mac is the authority: the local device store has already been
    /// written by the time this runs, and the helper enforces the grant on
    /// every request whatever the directory says. This exists so the phone,
    /// which reads its own permission from the directory, does not go on
    /// offering a terminal it will be refused — or hide one it now holds.
    func mirrorPermission(clientDeviceID: String, permission: DevicePermission) async throws {
        guard let address, let credentials = try store.load(),
              credentials.address == address.absoluteString else {
            throw ControlPlaneHostError.notConfigured
        }
        try await apiFactory(address).setPairingPermission(
            deviceToken: credentials.deviceToken,
            clientDeviceID: clientDeviceID,
            permission: permission
        )
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

    /// Publishes only validated, non-loopback listener addresses. The service
    /// owns the final lifetime; callers schedule the next refresh from its
    /// returned TTL instead of assuming its deployment configuration.
    /// - Parameters:
    ///   - ice: the agent's credentials, or nil while this Mac has no ICE agent.
    ///   - agentCandidates: candidates the ICE agent gathered (server-reflexive
    ///     and relay). Empty until Phase E, which is why the host candidate is
    ///     built here rather than by a caller.
    func publishPresence(
        publicKey: String,
        macName: String,
        listenerAddress: String,
        interfaceHosts: [String] = [],
        ice: ControlPlaneIceCredentials? = nil,
        agentCandidates: [ControlPlaneCandidate] = [],
        now: Date = Date()
    ) async throws -> ControlPlanePresence {
        let listener = try Self.listenerCandidates(
            listenerAddress,
            interfaceHosts: interfaceHosts,
            now: now
        )
        // Validated here, before the round trip, so a malformed candidate is a
        // local error rather than a 400 the refresh loop retries into.
        let candidates = try (listener + agentCandidates).map { try $0.validatedForPublication() }
        guard candidates.count <= Self.maxCandidates else {
            throw ControlPlaneHostError.malformedResponse(
                "presence carries at most \(Self.maxCandidates) candidates"
            )
        }
        let credentials = try await enrollment(publicKey: publicKey, name: macName)
        guard let address else { throw ControlPlaneHostError.notConfigured }
        return try await apiFactory(address).publishPresence(
            deviceToken: credentials.deviceToken,
            candidates: candidates,
            ice: ice
        )
    }

    /// The service's default cap. Exceeding it is a 400, so it is enforced
    /// before the request rather than after.
    static let maxCandidates = 8

    /// The lifetime every candidate is published with, matching the service's
    /// default presence window. A candidate the helper gathered a minute or an
    /// hour ago is still the same address: its lifetime is a property of the
    /// presence record carrying it, so it is stamped at publication rather
    /// than copied from when the agent happened to gather.
    static let presenceLifetime: UInt64 = 90

    /// Best-effort deletion is used for lifecycle cleanup. A missed delete is
    /// still bounded by the service TTL, while the local incident switch has
    /// already stopped the listener before this network request is made.
    func clearPresence() async {
        guard let address, let credentials = try? store.load(),
              credentials.address == address.absoluteString else { return }
        try? await apiFactory(address).clearPresence(deviceToken: credentials.deviceToken)
    }

    /// Consumes the control plane's one-shot offer queue. The caller must
    /// still re-check every peer against the local device store before handing
    /// an offer to a transport; the service cannot authorize this Mac's local
    /// gateway on its own.
    func rendezvousOffers(wait: UInt64 = 0) async throws -> [ControlPlaneRendezvousOffer] {
        guard let address, let credentials = try store.load(),
              credentials.address == address.absoluteString else {
            throw ControlPlaneHostError.notConfigured
        }
        return try await apiFactory(address).rendezvousOffers(
            deviceToken: credentials.deviceToken,
            wait: min(wait, Self.maxRendezvousWait)
        )
    }

    /// The longest hold the service honours on one offer collection. Asking
    /// for more is a 400, so the cap is applied here rather than discovered.
    static let maxRendezvousWait: UInt64 = 25

    /// The STUN servers the helper should gather against, as URLs.
    ///
    /// Enrolls this Mac on demand, the same as opening a pairing code does:
    /// the helper is launched when remote access is turned on, which can be
    /// before any phone has paired, and a Mac with a control plane configured
    /// has already decided to appear in that directory. Relay entries are
    /// dropped here as well as refused by the helper — a TURN allocation is
    /// the phone's decision under its own credentials, never the Mac's.
    func stunServerURLs(publicKey: String, macName: String) async throws -> [String] {
        guard let address else { throw ControlPlaneHostError.notConfigured }
        let credentials = try await enrollment(publicKey: publicKey, name: macName)
        let servers = try await apiFactory(address).iceServers(deviceToken: credentials.deviceToken)
        var seen: Set<String> = []
        return servers
            .flatMap(\.urls)
            .filter(ControlPlaneIceServer.isStun)
            .filter { seen.insert($0).inserted }
    }

    /// Mirrors the local relay switch to the account-level relay issuer. This
    /// affects only relay credentials: presence remains available for LAN and
    /// direct paths during a relay incident.
    func setRelayEnabled(_ enabled: Bool) async throws {
        guard let address, let credentials = try store.load(),
              credentials.address == address.absoluteString else { return }
        try await apiFactory(address).setRelayEnabled(
            accountToken: credentials.accountToken,
            enabled: enabled
        )
    }

    /// Validates the stricter client subset of the service's candidate format.
    /// The service accepts only one to its configured cap (eight by default),
    /// literal addresses, and an expiry within its presence TTL. We additionally
    /// reject loopback because publishing an address a phone can only reach on
    /// itself is a privacy and correctness failure.
    static func presenceCandidate(
        _ address: String,
        now: Date = Date(),
        lifetime: UInt64 = 90
    ) throws -> ControlPlaneCandidate {
        guard lifetime > 0, lifetime <= 90,
              let split = splitAddress(address), isPublishableHost(split.host) else {
            throw ControlPlaneHostError.malformedResponse("listener address is not a publishable non-loopback IP literal")
        }
        let expiresAt = UInt64(now.timeIntervalSince1970) + lifetime
        // Described honestly as what it is: the helper's authenticated TCP
        // listener on this machine's own interface. Until the ICE agent lands
        // this is the only candidate, which is a host-only degenerate case of
        // the ICE shape rather than a different one.
        return ControlPlaneCandidate(
            address: split.normalized,
            expiresAt: expiresAt,
            type: "host",
            priority: hostPriority,
            foundation: foundation(for: split.host, protocol: "tcp", type: "host"),
            component: 1,
            protocol: "tcp",
            tcpType: "passive"
        )
    }

    /// Every address a phone can dial the authenticated TCP listener at.
    ///
    /// The helper binds `0.0.0.0`, so the address it reports names a port and
    /// no reachable host at all. The reachable ones come from the ICE agent's
    /// own gathered host candidates — which is exactly where a tailnet address
    /// already is, the 100.x one and the fd7a: one alike — and each is
    /// republished as the same TCP listener on the same port. That is what
    /// lets a tailnet work with no ICE involved: the phone dials one of these
    /// directly and runs the ordinary pinned Noise handshake over it.
    ///
    /// At most `maxListenerCandidates` are published. Presence carries eight
    /// candidates in total and the agent's reflexive ones have to fit beside
    /// these, so a Mac with many interfaces publishes its first few rather
    /// than crowding out the only path an off-tailnet phone has.
    static func listenerCandidates(
        _ listenerAddress: String,
        interfaceHosts: [String],
        now: Date = Date(),
        lifetime: UInt64 = 90
    ) throws -> [ControlPlaneCandidate] {
        guard let listener = splitAddress(listenerAddress) else {
            throw ControlPlaneHostError.malformedResponse(
                "listener address is not a publishable non-loopback IP literal"
            )
        }
        var hosts: [String] = []
        // The bound host first when it is a real one; a specific bind is the
        // owner having chosen an interface, and it should keep its place.
        if isPublishableHost(listener.host) { hosts.append(listener.host) }
        for host in interfaceHosts where isPublishableHost(host) && !hosts.contains(host) {
            hosts.append(host)
        }
        let candidates = try hosts.prefix(maxListenerCandidates).map { host in
            try presenceCandidate(
                joined(host: host, port: listener.port),
                now: now,
                lifetime: lifetime
            )
        }
        guard !candidates.isEmpty else {
            // Reached when the listener is bound to every interface and the
            // helper reported none of them — a helper running without an ICE
            // agent, which is where the interface addresses come from. There
            // is nothing to publish; saying so beats advertising `0.0.0.0`,
            // which is an address no phone can dial.
            throw ControlPlaneHostError.malformedResponse(
                "this Mac's listener has no address a phone could reach"
            )
        }
        return candidates
    }

    /// How many of the listener's interfaces presence advertises.
    static let maxListenerCandidates = 4

    private static func joined(host: String, port: Int) -> String {
        isIPv6Literal(host) ? "[\(host)]:\(port)" : "\(host):\(port)"
    }

    /// Whether an address names something a phone could route to.
    ///
    /// Loopback reaches only the Mac itself, and the unspecified address names
    /// no interface at all — publishing `0.0.0.0` as a candidate hands a phone
    /// something it can never dial.
    nonisolated static func isPublishableHost(_ host: String) -> Bool {
        !isLoopbackHost(host) && !isUnspecifiedHost(host)
    }

    nonisolated static func isUnspecifiedHost(_ host: String) -> Bool {
        if host == "0.0.0.0" || host == "::" { return true }
        return host.lowercased() == "0:0:0:0:0:0:0:0"
    }

    /// RFC 8445 §5.1.2.1: `(2^24)·type + (2^8)·local + (256 − component)`, with
    /// the host type preference of 126 and a single local interface.
    private static let hostPriority: UInt32 = (126 << 24) | (65_535 << 8) | (256 - 1)

    /// ICE requires candidates that share a type, base, and transport to share
    /// a foundation, and distinct ones to differ. Deriving it from exactly
    /// those three inputs satisfies both halves, and keeps it stable across
    /// presence refreshes so a peer does not see the pairing re-form.
    nonisolated static func foundation(
        for host: String,
        protocol proto: String,
        type: String
    ) -> String {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in "\(type)|\(proto)|\(host)".utf8 {
            hash = (hash ^ UInt64(byte)) &* 0x100_0000_01b3
        }
        return String(hash, radix: 36)
    }

    private static func splitAddress(_ value: String) -> (host: String, port: Int, normalized: String)? {
        if value.hasPrefix("[") {
            guard let close = value.firstIndex(of: "]"),
                  value.index(after: close) < value.endIndex,
                  value[value.index(after: close)] == ":",
                  let port = Int(value[value.index(close, offsetBy: 2)...]),
                  (1...65_535).contains(port) else { return nil }
            let host = String(value[value.index(after: value.startIndex)..<close])
            guard isIPv6Literal(host) else { return nil }
            return (host, port, "[\(host)]:\(port)")
        }
        guard let colon = value.lastIndex(of: ":"),
              let port = Int(value[value.index(after: colon)...]),
              (1...65_535).contains(port) else { return nil }
        let host = String(value[..<colon])
        guard isIPv4Literal(host) else { return nil }
        return (host, port, "\(host):\(port)")
    }

    nonisolated private static func isIPv4Literal(_ value: String) -> Bool {
        let parts = value.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy {
            guard let octet = Int($0), String(octet) == $0 || $0 == "0" else { return false }
            return (0...255).contains(octet)
        }
    }

    private static func isIPv6Literal(_ value: String) -> Bool {
        // The control plane performs the full wire-format check. This only
        // prevents hostnames and obviously malformed values from leaving the
        // Mac without pulling a network stack into the desktop adapter.
        !value.isEmpty && value.allSatisfy { $0.isHexDigit || $0 == ":" }
    }

    nonisolated static func isLoopbackHost(_ host: String) -> Bool {
        if host == "::1" || host.lowercased() == "0:0:0:0:0:0:0:1" { return true }
        guard isIPv4Literal(host) else { return false }
        return host.split(separator: ".").first == "127"
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
