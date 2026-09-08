import Foundation

public struct RemoteEnrollmentClaim: Decodable, Equatable, Sendable {
    public let version: Int
    public let enrollmentId: String
    public let provisionalDeviceId: String
    public let provisionalToken: String
    public let relayUrl: URL
    public let controllerAdmission: String
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
    public init(device: Device, mac: Mac, accessToken: String? = nil) {
        self.device = device
        self.mac = mac
        self.accessToken = accessToken
    }
}

/// Why a control-plane call failed.
public enum ControlPlaneError: Error, Equatable, Sendable, LocalizedError {
    /// The secret or the access token was rejected. Not retryable.
    case rejected(String)
    /// Any other non-2xx answer.
    case http(status: Int, path: String, reason: String)
    /// The answer did not match the contract.
    case malformedResponse(String)
    /// The network failed.
    case transport(String)

    public var message: String {
        switch self {
        case .rejected(let reason):
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
        case .transport, .http: return true
        default: return false
        }
    }
}

/// Device-authenticated control-plane maintenance operations.
public protocol ControlPlaneClient: Sendable {
    /// Re-reads the device record, which is how the phone notices a revoke or
    /// a permission change made on the Mac.
    func device(deviceId: String, accessToken: String) async throws -> PairingConfirmation
    /// Revokes this phone from the phone's side.
    func revoke(deviceId: String, accessToken: String) async throws
    /// Registers the opaque APNs token so the Mac can ask for a generic
    /// attention alert. The token is the only thing sent.
    func registerPush(token: String, accessToken: String) async throws
    func unregisterPush(accessToken: String) async throws
}

/// HTTP implementation of Remote Link enrollment and control operations.
public actor HTTPControlPlaneClient: ControlPlaneClient, SignalingClient {
    private let baseURL: URL
    private let session: URLSession
    private let decoder = JSONDecoder()

    public init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    public func claimRemoteEnrollment(
        enrollmentId: String,
        admissionCode: String,
        name: String,
        publicKey: String
    ) async throws -> RemoteEnrollmentClaim {
        let body = try JSONSerialization.data(withJSONObject: [
            "admissionCode": admissionCode,
            "name": name,
            "platform": "ios",
            "publicKey": publicKey,
        ])
        return try await request(
            path: "/v1/enrollments/\(escape(enrollmentId))/claim",
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

    public func registerPush(token: String, accessToken: String) async throws {
        let body = try JSONSerialization.data(withJSONObject: ["pushToken": token])
        let _: EmptyResponse = try await request(
            path: "/v1/push-registrations",
            method: "PUT",
            body: body,
            accessToken: accessToken
        )
    }

    public func unregisterPush(accessToken: String) async throws {
        let _: EmptyResponse = try await request(
            path: "/v1/push-registrations",
            method: "DELETE",
            body: nil,
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

    /// Turns a control-plane error body into a typed failure.
    static func error(status: Int, path: String, data: Data) -> ControlPlaneError {
        let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        let code = body?["error"] as? String
        let reason = body?["reason"] as? String ?? code ?? ""
        switch status {
        case 401, 403, 404, 409, 410:
            return .rejected(
                reason.isEmpty
                    ? "The control plane rejected this Remote Link operation."
                    : reason
            )
        default:
            return .http(
                status: status,
                path: path,
                reason: reason.isEmpty ? "request failed" : reason
            )
        }
    }

}
