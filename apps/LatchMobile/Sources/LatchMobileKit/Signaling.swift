import Foundation

/// Service-side windows the phone uses as a send-side preflight.
///
/// The control plane is authoritative if a deployment changes these; a 400
/// is the remaining backstop. Matching the deployed defaults means the phone
/// does not publish a body the service will refuse.
public enum SignalingWindows {
    /// `presenceTtlSeconds` on the control plane.
    public static let presenceTTL: UInt64 = 90
    /// `rendezvousTtlSeconds` on the control plane.
    public static let rendezvousTTL: UInt64 = 60
    /// `maxCandidates` on the control plane.
    public static let maxCandidates = 8
}

/// A short-lived transport address. It is an IP literal and a port with a
/// bounded absolute expiry: never a hostname, never a scheme, and never the
/// loopback gateway address or its per-launch credential.
public struct TransportCandidate: Codable, Equatable, Sendable {
    public let address: String
    public let expiresAt: UInt64
    /// RFC 8445 candidate type. ICE-aware publishers provide every metadata
    /// field below together; older host-candidate publishers omit all of them.
    public let type: String?
    public let priority: UInt32?
    public let foundation: String?
    public let component: Int?
    public let `protocol`: String?
    public let relatedAddress: String?
    public let relatedPort: Int?
    public let tcpType: String?

    public init(
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

    /// Builds a candidate the service will accept, matching the Mac's
    /// `presenceCandidate` preflight: literal address, non-loopback, expiry
    /// inside `lifetime`.
    public static func shortLived(
        address: String,
        now: Date = Date(),
        lifetime: UInt64 = SignalingWindows.presenceTTL
    ) throws -> TransportCandidate {
        let candidate = TransportCandidate(
            address: address,
            expiresAt: UInt64(now.timeIntervalSince1970) + lifetime
        )
        return try candidate.validated(now: now, maxLifetime: lifetime)
    }

    /// Send-side check used before presence and rendezvous. Receive-side
    /// decoding does not run this: a peer's candidates are consumed as
    /// advertised, then tried.
    public func validated(
        now: Date = Date(),
        maxLifetime: UInt64
    ) throws -> TransportCandidate {
        guard maxLifetime > 0, let split = Self.splitAddress(address) else {
            throw ControlPlaneError.invalidCandidate(
                "\(address) is not a publishable IP literal and port."
            )
        }
        if Self.isLoopbackHost(split.host) {
            throw ControlPlaneError.invalidCandidate(
                "\(address) is a loopback address; the control plane must never see one."
            )
        }
        let nowSeconds = UInt64(now.timeIntervalSince1970)
        if expiresAt <= nowSeconds || expiresAt > nowSeconds + maxLifetime {
            throw ControlPlaneError.invalidCandidate(
                "Candidate expiry must fall within \(maxLifetime) seconds."
            )
        }
        let metadata = [
            type != nil, priority != nil, foundation != nil, component != nil, `protocol` != nil,
        ]
        if metadata.contains(true), metadata.contains(false) {
            throw ControlPlaneError.invalidCandidate(
                "An ICE candidate must carry type, priority, foundation, component, and protocol together."
            )
        }
        if let type, !Self.iceTypes.contains(type) {
            throw ControlPlaneError.invalidCandidate("\(type) is not an ICE candidate type.")
        }
        if let proto = `protocol`, !Self.iceProtocols.contains(proto) {
            throw ControlPlaneError.invalidCandidate("\(proto) is not an ICE transport.")
        }
        if let foundation,
           foundation.isEmpty || foundation.count > 32 || !foundation.allSatisfy({
               $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "+" || $0 == "-" || $0 == "_")
           }) {
            throw ControlPlaneError.invalidCandidate("An ICE foundation is 1 to 32 safe characters.")
        }
        if let component, component != 1, component != 2 {
            throw ControlPlaneError.invalidCandidate("An ICE component is 1 or 2.")
        }
        if (relatedAddress == nil) != (relatedPort == nil) {
            throw ControlPlaneError.invalidCandidate(
                "relatedAddress and relatedPort are published together or not at all."
            )
        }
        if let relatedAddress,
           !Self.isIPLiteral(relatedAddress) || Self.isLoopbackHost(relatedAddress) {
            throw ControlPlaneError.invalidCandidate(
                "A related address must be a non-loopback IP literal."
            )
        }
        if let relatedPort, !(1...65_535).contains(relatedPort) {
            throw ControlPlaneError.invalidCandidate("A related port is between 1 and 65535.")
        }
        if let tcpType {
            guard Self.iceTCPTypes.contains(tcpType) else {
                throw ControlPlaneError.invalidCandidate("\(tcpType) is not a TCP candidate type.")
            }
            guard `protocol` == "tcp" else {
                throw ControlPlaneError.invalidCandidate("tcpType applies only to TCP candidates.")
            }
        }
        return TransportCandidate(
            address: split.normalized,
            expiresAt: expiresAt,
            type: type,
            priority: priority,
            foundation: foundation,
            component: component,
            protocol: `protocol`,
            relatedAddress: relatedAddress,
            relatedPort: relatedPort,
            tcpType: tcpType
        )
    }

    /// Validates a set the phone is about to send. Empty and over-long lists
    /// fail here so the service 400 is not the first signal.
    public static func prepare(
        _ candidates: [TransportCandidate],
        now: Date = Date(),
        maxCount: Int = SignalingWindows.maxCandidates,
        maxLifetime: UInt64
    ) throws -> [TransportCandidate] {
        guard !candidates.isEmpty else {
            throw ControlPlaneError.invalidCandidate("At least one candidate is required.")
        }
        guard candidates.count <= maxCount else {
            throw ControlPlaneError.invalidCandidate(
                "At most \(maxCount) candidates can be published at once."
            )
        }
        return try candidates.map { try $0.validated(now: now, maxLifetime: maxLifetime) }
    }

    /// Reduces an ICE agent's gathered routes to the control-plane limit.
    ///
    /// Candidate priority supplies the normal ICE ordering, but taking only a
    /// priority-sorted prefix can discard every lower-priority reflexive or
    /// relay route when a device has many host interfaces. Reserve the best
    /// candidate of each type first, then each type/transport combination,
    /// before filling remaining slots by priority. The subsequent `prepare`
    /// call remains the authority for validation and rejecting other callers
    /// that fail to bound their own payloads.
    public static func preferredForPublication(
        _ candidates: [TransportCandidate],
        maxCount: Int = SignalingWindows.maxCandidates
    ) -> [TransportCandidate] {
        guard maxCount > 0, candidates.count > maxCount else {
            return maxCount > 0 ? candidates : []
        }

        let ranked = candidates.enumerated().sorted { left, right in
            let leftPriority = left.element.priority ?? 0
            let rightPriority = right.element.priority ?? 0
            if leftPriority != rightPriority { return leftPriority > rightPriority }
            return left.offset < right.offset
        }
        var selectedOffsets = Set<Int>()
        var selected: [TransportCandidate] = []

        func append(_ entry: (offset: Int, element: TransportCandidate)) {
            guard selected.count < maxCount, selectedOffsets.insert(entry.offset).inserted else {
                return
            }
            selected.append(entry.element)
        }

        var types = Set<String>()
        for entry in ranked where selected.count < maxCount {
            if types.insert(entry.element.type ?? "unknown").inserted {
                append(entry)
            }
        }

        var typeTransports = Set(
            selected.map { "\($0.type ?? "unknown")|\($0.protocol ?? "unknown")" }
        )
        for entry in ranked where selected.count < maxCount {
            let key = "\(entry.element.type ?? "unknown")|\(entry.element.protocol ?? "unknown")"
            if typeTransports.insert(key).inserted {
                append(entry)
            }
        }

        for entry in ranked where selected.count < maxCount {
            append(entry)
        }
        return selected
    }

    /// A rendezvous `requestId` the control plane will accept.
    public static func requestId() -> String {
        "rdv-\(UUID().uuidString)"
    }

    func jsonObject() -> [String: Any] {
        var object: [String: Any] = ["address": address, "expiresAt": Int(expiresAt)]
        if let type { object["type"] = type }
        if let priority { object["priority"] = priority }
        if let foundation { object["foundation"] = foundation }
        if let component { object["component"] = component }
        if let proto = `protocol` { object["protocol"] = proto }
        if let relatedAddress { object["relatedAddress"] = relatedAddress }
        if let relatedPort { object["relatedPort"] = relatedPort }
        if let tcpType { object["tcpType"] = tcpType }
        return object
    }

    static func splitAddress(_ value: String) -> (host: String, normalized: String)? {
        if value.hasPrefix("[") {
            guard let close = value.firstIndex(of: "]"),
                  value.index(after: close) < value.endIndex,
                  value[value.index(after: close)] == ":",
                  let port = Int(value[value.index(close, offsetBy: 2)...]),
                  (1...65_535).contains(port)
            else { return nil }
            let host = String(value[value.index(after: value.startIndex)..<close])
            guard isIPv6Literal(host) else { return nil }
            return (host, "[\(host)]:\(port)")
        }
        guard let colon = value.lastIndex(of: ":"),
              let port = Int(value[value.index(after: colon)...]),
              (1...65_535).contains(port)
        else { return nil }
        let host = String(value[..<colon])
        guard isIPv4Literal(host) else { return nil }
        return (host, "\(host):\(port)")
    }

    private static func isIPv4Literal(_ value: String) -> Bool {
        let parts = value.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy {
            guard let octet = Int($0), String(octet) == $0 || $0 == "0" else { return false }
            return (0...255).contains(octet)
        }
    }

    private static func isIPv6Literal(_ value: String) -> Bool {
        !value.isEmpty && value.allSatisfy { $0.isHexDigit || $0 == ":" }
    }

    private static func isIPLiteral(_ value: String) -> Bool {
        isIPv4Literal(value) || isIPv6Literal(value)
    }

    private static func isLoopbackHost(_ host: String) -> Bool {
        if host == "::1" || host.lowercased() == "0:0:0:0:0:0:0:1" { return true }
        guard isIPv4Literal(host) else { return false }
        return host.split(separator: ".").first == "127"
    }

    private static let iceTypes = ["host", "srflx", "prflx", "relay"]
    private static let iceProtocols = ["udp", "tcp"]
    private static let iceTCPTypes = ["active", "passive", "so"]
}

/// What `POST /v1/presence` returns. The TTL is how the caller schedules the
/// next refresh; it does not invent one.
public struct PublishedPresence: Decodable, Equatable, Sendable {
    public let deviceId: String
    public let expiresAt: UInt64
    public let ttlSeconds: UInt64

    public init(deviceId: String, expiresAt: UInt64, ttlSeconds: UInt64) {
        self.deviceId = deviceId
        self.expiresAt = expiresAt
        self.ttlSeconds = ttlSeconds
    }
}

/// A paired peer's current presence, including the offline shape the service
/// returns rather than a 404.
public struct PeerPresence: Decodable, Equatable, Sendable {
    public let deviceId: String
    public let online: Bool
    public let identityKey: String?
    public let candidates: [TransportCandidate]
    /// The peer's ephemeral ICE credentials. They authenticate ICE checks;
    /// they are not this phone's pinned Noise identity.
    public let iceUfrag: String?
    public let icePwd: String?
    public let expiresAt: UInt64?

    public init(
        deviceId: String,
        online: Bool,
        identityKey: String? = nil,
        candidates: [TransportCandidate] = [],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        expiresAt: UInt64? = nil
    ) {
        self.deviceId = deviceId
        self.online = online
        self.identityKey = identityKey
        self.candidates = candidates
        self.iceUfrag = iceUfrag
        self.icePwd = icePwd
        self.expiresAt = expiresAt
    }
}

/// The Mac's answer to an offer: its candidates, the permission currently
/// granted to this phone, and the identity the control plane claims. The pin
/// that authorizes a handshake is `PairedDeviceRecord.mac.publicKey`, not
/// `peerIdentityKey`.
public struct RendezvousAnswer: Decodable, Equatable, Sendable {
    public let requestId: String
    public let peerDeviceId: String
    public let peerIdentityKey: String
    public let permission: DevicePermission
    public let candidates: [TransportCandidate]
    public let iceUfrag: String?
    public let icePwd: String?
    public let expiresAt: UInt64

    public init(
        requestId: String,
        peerDeviceId: String,
        peerIdentityKey: String,
        permission: DevicePermission,
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        expiresAt: UInt64
    ) {
        self.requestId = requestId
        self.peerDeviceId = peerDeviceId
        self.peerIdentityKey = peerIdentityKey
        self.permission = permission
        self.candidates = candidates
        self.iceUfrag = iceUfrag
        self.icePwd = icePwd
        self.expiresAt = expiresAt
    }

    private enum CodingKeys: String, CodingKey {
        case requestId, peerDeviceId, peerIdentityKey, permission, candidates, iceUfrag, icePwd, expiresAt
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        requestId = try container.decode(String.self, forKey: .requestId)
        peerDeviceId = try container.decode(String.self, forKey: .peerDeviceId)
        peerIdentityKey = try container.decode(String.self, forKey: .peerIdentityKey)
        permission = DevicePermission.granted(
            try container.decodeIfPresent(String.self, forKey: .permission)
        )
        candidates = try container.decode([TransportCandidate].self, forKey: .candidates)
        iceUfrag = try container.decodeIfPresent(String.self, forKey: .iceUfrag)
        icePwd = try container.decodeIfPresent(String.self, forKey: .icePwd)
        expiresAt = try container.decode(UInt64.self, forKey: .expiresAt)
    }

    /// Cheap early abort before a handshake. A match here never authorizes;
    /// a mismatch means the service named a different Mac than pairing pinned.
    public func matchesPinnedKey(_ pinned: String) -> Bool {
        peerIdentityKey == pinned
    }
}

/// An inbound offer collected by `GET /v1/rendezvous`. Consumed by the
/// service as it is returned, so a second collect is empty.
public struct RendezvousOffer: Decodable, Equatable, Sendable {
    public let requestId: String
    public let peerDeviceId: String
    public let peerIdentityKey: String
    public let candidates: [TransportCandidate]
    public let iceUfrag: String?
    public let icePwd: String?
    public let expiresAt: UInt64

    public init(
        requestId: String,
        peerDeviceId: String,
        peerIdentityKey: String,
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        expiresAt: UInt64
    ) {
        self.requestId = requestId
        self.peerDeviceId = peerDeviceId
        self.peerIdentityKey = peerIdentityKey
        self.candidates = candidates
        self.iceUfrag = iceUfrag
        self.icePwd = icePwd
        self.expiresAt = expiresAt
    }
}

/// Short-lived ICE servers from `POST /v1/turn-credentials`. Requested only
/// after a direct path has already failed; the account relay switch can
/// refuse this even when presence is still published.
public struct TurnCredentials: Decodable, Equatable, Sendable {
    public let iceServers: [IceServer]
    public let expiresAt: UInt64

    public init(iceServers: [IceServer], expiresAt: UInt64) {
        self.iceServers = iceServers
        self.expiresAt = expiresAt
    }
}

/// One ICE server entry. `urls` is accepted as either a string or an array
/// because both shapes appear in ICE configuration.
public struct IceServer: Decodable, Equatable, Sendable {
    public let urls: [String]
    public let username: String?
    public let credential: String?

    public init(urls: [String], username: String? = nil, credential: String? = nil) {
        self.urls = urls
        self.username = username
        self.credential = credential
    }

    private enum CodingKeys: String, CodingKey {
        case urls, username, credential
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let list = try? container.decode([String].self, forKey: .urls) {
            urls = list
        } else {
            urls = [try container.decode(String.self, forKey: .urls)]
        }
        username = try container.decodeIfPresent(String.self, forKey: .username)
        credential = try container.decodeIfPresent(String.self, forKey: .credential)
    }
}

/// The phone's half of presence, rendezvous, and TURN credential issuance.
///
/// Pairing stays on `ControlPlaneClient`. This surface authenticates with the
/// same device bearer and reuses `ControlPlaneError` so a failure is a
/// sentence the UI can show rather than a status code.
public protocol SignalingClient: Sendable {
    func publishPresence(
        candidates: [TransportCandidate],
        iceUfrag: String?,
        icePwd: String?,
        accessToken: String
    ) async throws -> PublishedPresence

    func clearPresence(accessToken: String) async throws

    func presence(deviceId: String, accessToken: String) async throws -> PeerPresence

    func offerRendezvous(
        targetDeviceId: String,
        candidates: [TransportCandidate],
        iceUfrag: String?,
        icePwd: String?,
        accessToken: String,
        requestId: String,
        expiresAt: UInt64?
    ) async throws -> RendezvousAnswer

    func collectOffers(accessToken: String) async throws -> [RendezvousOffer]

    /// STUN-only server configuration for gathering direct candidates before
    /// a peer is known. This deliberately remains available if relay fallback
    /// is disabled; TURN URLs stay in `turnCredentials` below.
    func iceServers(accessToken: String) async throws -> [IceServer]

    func turnCredentials(peerDeviceId: String, accessToken: String) async throws -> TurnCredentials
}

extension SignalingClient {
    /// Reachability of the paired Mac. A pairing without a control-plane Mac
    /// id is the manual `latch serve` path.
    public func macPresence(for record: PairedDeviceRecord) async throws -> PeerPresence {
        try await presence(
            deviceId: record.signalingMacDeviceId(),
            accessToken: record.signalingAccessToken()
        )
    }

    public func offerRendezvous(
        for record: PairedDeviceRecord,
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        requestId: String = TransportCandidate.requestId(),
        expiresAt: UInt64? = nil
    ) async throws -> RendezvousAnswer {
        try await offerRendezvous(
            targetDeviceId: record.signalingMacDeviceId(),
            candidates: candidates,
            iceUfrag: iceUfrag,
            icePwd: icePwd,
            accessToken: record.signalingAccessToken(),
            requestId: requestId,
            expiresAt: expiresAt
        )
    }

    public func collectOffers(for record: PairedDeviceRecord) async throws -> [RendezvousOffer] {
        try await collectOffers(accessToken: record.signalingAccessToken())
    }

    public func turnCredentials(for record: PairedDeviceRecord) async throws -> TurnCredentials {
        try await turnCredentials(
            peerDeviceId: record.signalingMacDeviceId(),
            accessToken: record.signalingAccessToken()
        )
    }

    public func iceServers(for record: PairedDeviceRecord) async throws -> [IceServer] {
        try await iceServers(accessToken: record.signalingAccessToken())
    }

    public func publishPresence(
        for record: PairedDeviceRecord,
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil
    ) async throws -> PublishedPresence {
        try await publishPresence(
            candidates: candidates,
            iceUfrag: iceUfrag,
            icePwd: icePwd,
            accessToken: record.signalingAccessToken()
        )
    }

    public func clearPresence(for record: PairedDeviceRecord) async throws {
        try await clearPresence(accessToken: record.signalingAccessToken())
    }
}

extension HTTPControlPlaneClient {
    public func publishPresence(
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        accessToken: String
    ) async throws -> PublishedPresence {
        let prepared = try TransportCandidate.prepare(
            candidates,
            maxLifetime: SignalingWindows.presenceTTL
        )
        let ice = try validatedIceCredentials(ufrag: iceUfrag, pwd: icePwd)
        var json: [String: Any] = ["candidates": prepared.map { $0.jsonObject() }]
        if let ice {
            json["iceUfrag"] = ice.ufrag
            json["icePwd"] = ice.pwd
        }
        return try await send(
            path: "/v1/presence",
            method: "POST",
            json: json,
            accessToken: accessToken
        )
    }

    public func clearPresence(accessToken: String) async throws {
        let _: EmptyResponse = try await request(
            path: "/v1/presence",
            method: "DELETE",
            body: nil,
            accessToken: accessToken
        )
    }

    public func presence(deviceId: String, accessToken: String) async throws -> PeerPresence {
        try await request(
            path: "/v1/presence/\(escape(deviceId))",
            method: "GET",
            body: nil,
            accessToken: accessToken
        )
    }

    public func offerRendezvous(
        targetDeviceId: String,
        candidates: [TransportCandidate],
        iceUfrag: String? = nil,
        icePwd: String? = nil,
        accessToken: String,
        requestId: String,
        expiresAt: UInt64?
    ) async throws -> RendezvousAnswer {
        let prepared = try TransportCandidate.prepare(
            candidates,
            maxLifetime: SignalingWindows.rendezvousTTL
        )
        var json: [String: Any] = [
            "targetDeviceId": targetDeviceId,
            "requestId": requestId,
            "candidates": prepared.map { $0.jsonObject() }
        ]
        if let ice = try validatedIceCredentials(ufrag: iceUfrag, pwd: icePwd) {
            json["iceUfrag"] = ice.ufrag
            json["icePwd"] = ice.pwd
        }
        if let expiresAt {
            json["expiresAt"] = Int(expiresAt)
        }
        return try await send(
            path: "/v1/rendezvous",
            method: "POST",
            json: json,
            accessToken: accessToken
        )
    }

    public func collectOffers(accessToken: String) async throws -> [RendezvousOffer] {
        struct Response: Decodable { let offers: [RendezvousOffer] }
        let response: Response = try await request(
            path: "/v1/rendezvous",
            method: "GET",
            body: nil,
            accessToken: accessToken
        )
        return response.offers
    }

    public func iceServers(accessToken: String) async throws -> [IceServer] {
        struct Response: Decodable { let iceServers: [IceServer] }
        let response: Response = try await request(
            path: "/v1/ice-servers",
            method: "GET",
            body: nil,
            accessToken: accessToken
        )
        return response.iceServers
    }

    public func turnCredentials(
        peerDeviceId: String,
        accessToken: String
    ) async throws -> TurnCredentials {
        try await send(
            path: "/v1/turn-credentials",
            method: "POST",
            json: ["peerDeviceId": peerDeviceId],
            accessToken: accessToken
        )
    }

    private func send<T: Decodable>(
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

    private func validatedIceCredentials(
        ufrag: String?,
        pwd: String?
    ) throws -> (ufrag: String, pwd: String)? {
        guard (ufrag == nil) == (pwd == nil) else {
            throw ControlPlaneError.invalidCandidate(
                "ICE ufrag and password must be supplied together."
            )
        }
        guard let ufrag, let pwd else { return nil }
        guard (4...256).contains(ufrag.count), ufrag.allSatisfy(Self.isICECredentialCharacter) else {
            throw ControlPlaneError.invalidCandidate("ICE ufrag is malformed.")
        }
        guard (22...256).contains(pwd.count), pwd.allSatisfy(Self.isICECredentialCharacter) else {
            throw ControlPlaneError.invalidCandidate("ICE password is malformed.")
        }
        return (ufrag, pwd)
    }

    private static func isICECredentialCharacter(_ character: Character) -> Bool {
        character.isASCII && (character.isLetter || character.isNumber || "+/_=-".contains(character))
    }
}
