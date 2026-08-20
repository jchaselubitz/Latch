import Foundation

/// What the phone sends to enroll itself against one scanned pairing code.
///
/// The device public key is the identity being enrolled; the secret is proof
/// that whoever is asking was in front of the unlocked Mac within the last five
/// minutes. The phrase is sent so the service can show the Mac the same words
/// the phone is showing — it is not a credential and proves nothing on its own.
public struct PairingEnrollment: Equatable, Sendable {
    public let pairingId: String
    public let secret: String
    public let devicePublicKey: String
    public let deviceName: String
    public let phrase: String

    public init(
        pairingId: String,
        secret: String,
        devicePublicKey: String,
        deviceName: String,
        phrase: String
    ) {
        self.pairingId = pairingId
        self.secret = secret
        self.devicePublicKey = devicePublicKey
        self.deviceName = deviceName
        self.phrase = phrase
    }

    var body: [String: Any] {
        [
            "formatVersion": PairingPayload.supportedFormatVersion,
            "secret": secret,
            "device": [
                "publicKey": devicePublicKey,
                "name": deviceName,
                "platform": "ios"
            ],
            "phrase": phrase
        ]
    }
}

/// What the control plane returns once the Mac has confirmed the phone.
public struct PairingConfirmation: Decodable, Equatable, Sendable {
    public struct Device: Decodable, Equatable, Sendable {
        public let deviceId: String
        public let name: String
        public let permission: DevicePermission
        public let revoked: Bool

        private enum CodingKeys: String, CodingKey {
            case deviceId, name, permission, revoked
        }

        public init(deviceId: String, name: String, permission: DevicePermission, revoked: Bool) {
            self.deviceId = deviceId
            self.name = name
            self.permission = permission
            self.revoked = revoked
        }

        public init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            deviceId = try container.decode(String.self, forKey: .deviceId)
            name = try container.decode(String.self, forKey: .name)
            // An unrecognized permission degrades to the least privilege
            // rather than to the default: a grant this build cannot model is
            // not a grant it may assume is generous.
            permission = DevicePermission.granted(
                try container.decodeIfPresent(String.self, forKey: .permission)
            )
            revoked = try container.decodeIfPresent(Bool.self, forKey: .revoked) ?? false
        }
    }

    public struct Mac: Decodable, Equatable, Sendable {
        public let deviceId: String?
        public let publicKey: String
        public let name: String?

        public init(deviceId: String? = nil, publicKey: String, name: String? = nil) {
            self.deviceId = deviceId
            self.publicKey = publicKey
            self.name = name
        }
    }

    public let device: Device
    public let mac: Mac
    /// Short-lived credential for later control-plane calls.
    public let accessToken: String?
    /// The phrase the Mac displayed, when the service echoes it back.
    public let phrase: String?

    public init(device: Device, mac: Mac, accessToken: String? = nil, phrase: String? = nil) {
        self.device = device
        self.mac = mac
        self.accessToken = accessToken
        self.phrase = phrase
    }
}

/// Why a control-plane call failed.
public enum ControlPlaneError: Error, Equatable, Sendable, LocalizedError {
    /// No control-plane address is known for this pairing.
    case noAddress
    /// The pairing record is gone: consumed, cancelled, or never existed.
    case pairingUnavailable
    /// The five minutes are up on the service's clock too.
    case pairingExpired
    /// This identity is already enrolled with the Mac.
    case alreadyPaired
    /// The secret or the access token was rejected. Not retryable.
    case rejected(String)
    /// The Mac's key in the answer is not the key in the QR code. Either the
    /// service is lying about which Mac this is, or something is in the middle.
    case identityMismatch(expected: String, received: String)
    /// The paired Mac has no current presence. Retryable: it may come online
    /// inside the next presence window.
    case macNotReachable(String)
    /// Relay credentials were refused because the account kill switch is off.
    /// Direct and LAN paths are unaffected.
    case relayDisabled(String)
    /// This pairing has no control-plane Mac id, so only a typed `latch serve`
    /// address can reach it.
    case manualLinkOnly
    /// A candidate the phone was about to publish would be rejected.
    case invalidCandidate(String)
    /// Any other non-2xx answer.
    case http(status: Int, path: String, reason: String)
    /// The answer did not match the contract.
    case malformedResponse(String)
    /// The network failed.
    case transport(String)

    public var message: String {
        switch self {
        case .noAddress:
            return "This pairing code does not say where to enroll, and no address was given."
        case .pairingUnavailable:
            return "Your Mac has already used or cancelled this pairing code. Show a new one."
        case .pairingExpired:
            return "This pairing code expired. Show a new one on your Mac."
        case .alreadyPaired:
            return "This phone is already paired with that Mac. Remove it there first."
        case .rejected(let reason):
            return reason
        case .identityMismatch(let expected, let received):
            return """
            This Mac answered with identity \(HexCoding.abbreviate(received)), but the code \
            was for \(HexCoding.abbreviate(expected)). Pairing stopped; do not confirm it.
            """
        case .macNotReachable(let reason):
            let detail = reason.isEmpty ? "it has not published a way to reach it" : reason
            return "Your Mac is not reachable right now: \(detail)."
        case .relayDisabled(let reason):
            return reason.isEmpty
                ? "Relay is disabled for this account. Direct or LAN access still works if your Mac is on the same network."
                : reason
        case .manualLinkOnly:
            return "This pairing has no Mac to reach over the control plane. Link with a typed latch serve address instead."
        case .invalidCandidate(let reason):
            return reason
        case .http(let status, let path, let reason):
            return "\(path) failed (\(status)): \(reason)"
        case .malformedResponse(let detail):
            return "The control plane sent an unexpected response: \(detail)"
        case .transport(let detail):
            return detail
        }
    }

    /// Preserve the recovery-oriented message when this error crosses a
    /// generic `Error` boundary, including the phone's loopback HTTP proxy.
    /// Without `LocalizedError`, Foundation renders only an opaque enum case
    /// number such as `ControlPlaneError error 4` and discards the reason the
    /// control plane returned.
    public var errorDescription: String? { message }

    /// Whether trying the same call again could plausibly work. Authentication
    /// and authorization failures are explicit and non-retryable, per the
    /// remote-access failure rules. A missing presence window is the exception:
    /// the Mac may publish again on its next refresh.
    public var isRetryable: Bool {
        switch self {
        case .transport, .http, .macNotReachable: return true
        default: return false
        }
    }
}

/// The phone's half of the control-plane pairing API.
///
/// The control plane is deliberately thin: it enrolls a device against a
/// pairing record the Mac created, reports the permission the Mac granted, and
/// accepts a revocation. It never sees a gateway token, terminal bytes, or a
/// private key — see `docs/REMOTE_ACCESS_THREAT_MODEL.md`.
public protocol ControlPlaneClient: Sendable {
    /// Enrolls this phone against a scanned pairing code.
    func enroll(pairingId: String, enrollment: PairingEnrollment) async throws -> PairingConfirmation
    /// Re-reads the device record, which is how the phone notices a revoke or
    /// a permission change made on the Mac.
    func device(deviceId: String, accessToken: String) async throws -> PairingConfirmation
    /// Revokes this phone from the phone's side.
    func revoke(deviceId: String, accessToken: String) async throws
}

/// The HTTP implementation of pairing and signaling.
///
/// Pairing and signaling share one client because they share one credential:
/// the device bearer stored on `PairedDeviceRecord`. They stay separate
/// protocols so a pairing-only stub does not have to pretend to rendezvous.
public actor HTTPControlPlaneClient: ControlPlaneClient, SignalingClient {
    private let baseURL: URL
    private let session: URLSession
    private let decoder = JSONDecoder()

    public init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    public func enroll(
        pairingId: String,
        enrollment: PairingEnrollment
    ) async throws -> PairingConfirmation {
        let body = try JSONSerialization.data(withJSONObject: enrollment.body)
        return try await request(
            path: "/v1/pairings/\(escape(pairingId))/confirm",
            method: "POST",
            body: body,
            accessToken: nil
        )
    }

    public func device(deviceId: String, accessToken: String) async throws -> PairingConfirmation {
        try await request(
            path: "/v1/devices/\(escape(deviceId))",
            method: "GET",
            body: nil,
            accessToken: accessToken
        )
    }

    public func revoke(deviceId: String, accessToken: String) async throws {
        let _: EmptyResponse = try await request(
            path: "/v1/devices/\(escape(deviceId))/revoke",
            method: "POST",
            body: Data("{}".utf8),
            accessToken: accessToken
        )
    }

    /// A 204 or an empty object, for calls whose answer carries nothing.
    struct EmptyResponse: Decodable {
        init() {}
        init(from decoder: Decoder) throws {}
    }

    func escape(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? value
    }

    func request<T: Decodable>(
        path: String,
        method: String,
        body: Data?,
        accessToken: String?
    ) async throws -> T {
        guard let url = URL(string: baseURL.absoluteString + path) else {
            throw ControlPlaneError.transport("\(baseURL.absoluteString + path) is not a usable address")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let accessToken {
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        }
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw ControlPlaneError.transport(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw ControlPlaneError.malformedResponse("no HTTP status")
        }
        guard (200..<300).contains(http.statusCode) else {
            throw Self.error(status: http.statusCode, path: path, data: data)
        }
        if data.isEmpty, let empty = EmptyResponse() as? T {
            return empty
        }
        do {
            return try decoder.decode(T.self, from: data)
        } catch {
            throw ControlPlaneError.malformedResponse(String(describing: error))
        }
    }

    /// Turns a control-plane error body into the failure the UI should show.
    ///
    /// The status codes are distinguished because the recovery differs: an
    /// expired or consumed pairing means "show a new code on the Mac", while a
    /// rejected secret means the code did not come from that Mac at all.
    static func error(status: Int, path: String, data: Data) -> ControlPlaneError {
        let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        let code = body?["error"] as? String
        let reason = body?["reason"] as? String ?? code ?? ""
        switch status {
        case 401:
            return .rejected(
                reason.isEmpty
                    ? "The control plane rejected this pairing. The code may have been used already."
                    : reason
            )
        case 403:
            if Self.isRelayDisabled(code: code, reason: reason) {
                return .relayDisabled(
                    reason.isEmpty
                        ? "Relay is disabled for this account. Direct or LAN access still works if your Mac is on the same network."
                        : reason
                )
            }
            return .rejected(
                reason.isEmpty
                    ? "The control plane rejected this pairing. The code may have been used already."
                    : reason
            )
        case 404:
            return .pairingUnavailable
        case 409:
            if code == "already_paired" { return .alreadyPaired }
            if code == "target_offline" {
                return .macNotReachable(reason.isEmpty ? "target device has no current presence" : reason)
            }
            return .pairingUnavailable
        case 410:
            return .pairingExpired
        default:
            return .http(
                status: status,
                path: path,
                reason: reason.isEmpty ? "request failed" : reason
            )
        }
    }

    /// The account relay kill switch is a different recovery from a revoked
    /// pairing: the person should try LAN/direct, not scan a new code.
    private static func isRelayDisabled(code: String?, reason: String) -> Bool {
        let haystack = "\(code ?? "") \(reason)".lowercased()
        return haystack.contains("relay access is disabled") || haystack.contains("relay_disabled")
    }
}
