import Foundation

/// The short-lived admission needed to join one opaque relay session.
public struct RelayAdmission: Decodable, Equatable, Sendable {
    public let version: Int
    public let relayUrl: URL
    public let admission: String
    public let expiresAt: UInt64

    public init(version: Int, relayUrl: URL, admission: String, expiresAt: UInt64) {
        self.version = version
        self.relayUrl = relayUrl
        self.admission = admission
        self.expiresAt = expiresAt
    }
}

/// The control plane's current projection of an authenticated peer link.
public struct RemoteLinkDirectoryEntry: Decodable, Equatable, Sendable {
    public let version: Int
    public let linkId: String
    public let peerDeviceId: String
    public let peerPublicKey: String
    public let permission: String
    public let grantRevision: UInt64

    public init(
        version: Int,
        linkId: String,
        peerDeviceId: String,
        peerPublicKey: String,
        permission: String,
        grantRevision: UInt64
    ) {
        self.version = version
        self.linkId = linkId
        self.peerDeviceId = peerDeviceId
        self.peerPublicKey = peerPublicKey
        self.permission = permission
        self.grantRevision = grantRevision
    }
}

/// A signed extension for an already redeemed relay lease.
public struct RelayLeaseExtension: Decodable, Equatable, Sendable {
    public let leaseId: String
    public let expiresAt: UInt64
    public let claim: String

    public init(leaseId: String, expiresAt: UInt64, claim: String) {
        self.leaseId = leaseId
        self.expiresAt = expiresAt
        self.claim = claim
    }

    enum CodingKeys: String, CodingKey {
        case leaseId, expiresAt
        case claim = "extension"
    }
}

/// Device-authenticated Remote Link control-plane operations.
public protocol SignalingClient: Sendable {
    func relayAdmission(peerDeviceId: String, accessToken: String) async throws -> RelayAdmission
    func remoteLinks(accessToken: String) async throws -> [RemoteLinkDirectoryEntry]
    func renewRelayLease(leaseId: String, accessToken: String) async throws -> RelayLeaseExtension
}

extension SignalingClient {
    public func relayAdmission(for record: PairedDeviceRecord) async throws -> RelayAdmission {
        try await relayAdmission(
            peerDeviceId: record.signalingMacDeviceId(),
            accessToken: record.signalingAccessToken()
        )
    }
}

extension HTTPControlPlaneClient {
    public func relayAdmission(
        peerDeviceId: String,
        accessToken: String
    ) async throws -> RelayAdmission {
        try await sendRemoteLink(
            path: "/v1/relay-admissions",
            method: "POST",
            json: ["peerDeviceId": peerDeviceId],
            accessToken: accessToken
        )
    }

    public func remoteLinks(accessToken: String) async throws -> [RemoteLinkDirectoryEntry] {
        struct Response: Decodable { let links: [RemoteLinkDirectoryEntry] }
        let response: Response = try await request(
            path: "/v1/remote-links",
            method: "GET",
            body: nil,
            accessToken: accessToken
        )
        return response.links
    }

    public func renewRelayLease(
        leaseId: String,
        accessToken: String
    ) async throws -> RelayLeaseExtension {
        try await sendRemoteLink(
            path: "/v1/relay-leases/\(escape(leaseId))/renew",
            method: "POST",
            json: [:],
            accessToken: accessToken
        )
    }

    private func sendRemoteLink<T: Decodable>(
        path: String,
        method: String,
        json: [String: Any],
        accessToken: String
    ) async throws -> T {
        let body: Data
        do {
            body = try JSONSerialization.data(withJSONObject: json)
        } catch {
            throw ControlPlaneError.malformedResponse(String(describing: error))
        }
        return try await request(
            path: path,
            method: method,
            body: body,
            accessToken: accessToken
        )
    }
}
