import Foundation

/// What a paired phone is allowed to do once its authenticated transport is
/// established. The order is a strict ladder: `control` implies `interact`,
/// which implies `observe`. The CLI enforces this before anything reaches
/// the gateway; the app mirrors it so the UI can never offer an action the
/// device would be refused for.
enum DevicePermission: String, Codable, CaseIterable, Identifiable, Sendable {
    case observe
    case interact
    case control

    var id: String { rawValue }

    var label: String {
        switch self {
        case .observe: return "Observe"
        case .interact: return "Control"
        case .control: return "Control + Terminal"
        }
    }

    var detail: String {
        switch self {
        case .observe:
            return "Read sessions, conversations, and events."
        case .interact:
            return "Also send messages and resolve prompts."
        case .control:
            return "Control sessions and open the terminal, which takes it from whatever is showing it."
        }
    }

    /// Whether a device holding this permission may perform `required`.
    func permits(_ required: DevicePermission) -> Bool {
        rank >= required.rank
    }

    private var rank: Int {
        switch self {
        case .observe: return 0
        case .interact: return 1
        case .control: return 2
        }
    }
}

/// Lifecycle snapshot from `latch remote-access status --json`.
struct RemoteAccessStatus: Codable, Equatable, Sendable {
    let formatVersion: Int
    let enabled: Bool
    let relayEnabled: Bool
    /// Whether this Mac refuses the relay outright. Stricter than a relay
    /// that is merely off: presence is narrowed to host candidates too, so a
    /// phone is only ever handed addresses it can reach directly.
    ///
    /// Optional on the wire so a CLI that predates the switch still decodes.
    /// A missing field is the permissive answer, which is what that CLI does.
    let neverRelayFlag: Bool?
    /// Opaque identifier for this Mac, created the first time remote access is
    /// enabled and stored by the CLI (Keychain-backed private key on macOS).
    let deviceID: String?
    /// The Mac's pinned public identity. Phones verify it while pairing, so it
    /// is safe to show and to encode into pairing material.
    let publicKey: String?
    let keyGeneration: UInt64?
    let pairedDevices: Int
    let revokedDevices: Int
    /// The authenticated LAN listener the helper is advertising, if running.
    /// This is never the supervised `latch serve` gateway address.
    let listenerAddress: String?
    /// The running helper's ICE agent. Presence publishes these credentials and
    /// candidates; the app does not invent its own, because the helper is the
    /// process that answers the connectivity checks they authenticate.
    let ice: RemoteIceDescription?
    /// How many phones hold an authenticated stream right now. The helper
    /// counts these after the Noise handshake and the route authorization, so
    /// this is a number of paired phones and never a number of open sockets.
    ///
    /// Optional for the same reason as `neverRelayFlag`, and read as zero when
    /// absent: a CLI that does not report connections has not reported one.
    let activeConnectionCount: Int?

    enum CodingKeys: String, CodingKey {
        case formatVersion, enabled, relayEnabled, publicKey, keyGeneration
        case pairedDevices, revokedDevices, listenerAddress, ice
        case deviceID = "deviceId"
        case neverRelayFlag = "neverRelay"
        case activeConnectionCount = "activeConnections"
    }

    var neverRelay: Bool { neverRelayFlag ?? false }
    var activeConnections: Int { activeConnectionCount ?? 0 }

    /// A phone is connected and this Mac is serving it. The sleep assertion
    /// hangs off exactly this: remote access on, and someone actually there.
    var hasLiveConnection: Bool { enabled && activeConnections > 0 }

    static let unavailable = RemoteAccessStatus(
        formatVersion: 1,
        enabled: false,
        relayEnabled: false,
        neverRelayFlag: nil,
        deviceID: nil,
        publicKey: nil,
        keyGeneration: nil,
        pairedDevices: 0,
        revokedDevices: 0,
        listenerAddress: nil,
        ice: nil,
        activeConnectionCount: nil
    )
}

/// The helper's gathered ICE agent, as `remote-access status --json` reports it.
///
/// The password is a STUN short-term credential rather than a capability: it
/// authenticates connectivity checks and grants nothing on its own, which is
/// why it travels the owner-facing status channel and the supervised gateway's
/// bearer token never does.
struct RemoteIceDescription: Codable, Equatable, Sendable {
    let ufrag: String
    let password: String
    let candidates: [RemoteIceCandidate]
}

/// One gathered candidate in the control plane's published shape.
struct RemoteIceCandidate: Codable, Equatable, Sendable {
    let type: String
    let priority: UInt32
    let foundation: String
    let component: Int
    let `protocol`: String
    let address: String
    let relatedAddress: String?
    let relatedPort: UInt16?
    let tcpType: String?
    let expiresAt: UInt64

    /// The presence representation. The candidate's transport fields are
    /// republished exactly as the agent gathered them: rewriting a priority or
    /// a foundation here would make the phone's pair ordering disagree with
    /// the Mac's. Only the lifetime is the publisher's: the helper stamped one
    /// when it gathered, and an agent that has been idle longer than that
    /// still answers on the same ports. Copying the stale stamp would have the
    /// control plane refuse every refresh after the first window.
    func published(expiresAt: UInt64) -> ControlPlaneCandidate {
        ControlPlaneCandidate(
            address: address,
            expiresAt: expiresAt,
            type: type,
            priority: priority,
            foundation: foundation,
            component: component,
            protocol: self.protocol,
            relatedAddress: relatedAddress,
            relatedPort: relatedPort.map(Int.init),
            tcpType: tcpType
        )
    }
}

/// One approved rendezvous offer, in the shape `latch remote-access offer`
/// reads on stdin.
///
/// It is transport parameters and nothing else. The control plane cannot
/// authorize this Mac's gateway, which is why an offer only ever reaches the
/// helper after a fresh local device-state check, and why the helper still runs
/// the full Noise handshake against the local device store afterwards.
struct RemoteRendezvousOfferDocument: Encodable, Equatable, Sendable {
    let requestID: String
    let peerDeviceID: String
    let iceUfrag: String
    let icePwd: String
    let candidates: [Candidate]
    let expiresAt: UInt64

    enum CodingKeys: String, CodingKey {
        case iceUfrag, icePwd, candidates, expiresAt
        case requestID = "requestId"
        case peerDeviceID = "peerDeviceId"
    }

    struct Candidate: Encodable, Equatable, Sendable {
        let type: String
        let priority: UInt32
        let foundation: String
        let component: Int
        let `protocol`: String
        let address: String
        let relatedAddress: String?
        let relatedPort: Int?
        let tcpType: String?
        let expiresAt: UInt64

        /// A candidate the helper could not run checks against is dropped
        /// rather than sent as a partial record: the CLI requires the full ICE
        /// ordering metadata, and a peer that published none cannot be reached
        /// this way at all.
        init?(_ candidate: ControlPlaneCandidate) {
            guard let type = candidate.type, let priority = candidate.priority,
                  let foundation = candidate.foundation, let component = candidate.component,
                  let proto = candidate.protocol else { return nil }
            self.type = type
            self.priority = priority
            self.foundation = foundation
            self.component = component
            self.protocol = proto
            self.address = candidate.address
            self.relatedAddress = candidate.relatedAddress
            self.relatedPort = candidate.relatedPort
            self.tcpType = candidate.tcpType
            self.expiresAt = candidate.expiresAt
        }
    }

    /// Builds the document for an offer that already passed the local
    /// device-state check. Returns nil when the peer published no ICE agent, or
    /// no candidate carrying the metadata ICE needs — neither can produce a
    /// connection, so handing it to the helper would only burn its one agent.
    init?(_ offer: ControlPlaneRendezvousOffer) {
        guard let ufrag = offer.iceUfrag, let pwd = offer.icePwd else { return nil }
        let candidates = offer.candidates.compactMap(Candidate.init)
        guard !candidates.isEmpty else { return nil }
        self.requestID = offer.requestID
        self.peerDeviceID = offer.peerDeviceID
        self.iceUfrag = ufrag
        self.icePwd = pwd
        self.candidates = candidates
        self.expiresAt = offer.expiresAt
    }
}

struct RemoteDevice: Codable, Identifiable, Equatable, Sendable {
    var id: String { deviceID }
    let deviceID: String
    let name: String
    let permission: DevicePermission
    let revoked: Bool
    /// The phone's row in the control-plane directory, recorded when it
    /// enrolled. Absent for devices paired before the CLI recorded it, which
    /// only means a grant change here cannot be mirrored there.
    let controlPlaneDeviceID: String?

    enum CodingKeys: String, CodingKey {
        case name, permission, revoked
        case deviceID = "deviceId"
        case controlPlaneDeviceID = "controlPlaneDeviceId"
    }

    /// Whether this device may open a session's terminal.
    var allowsTerminal: Bool { permission == .control }

    /// What the device keeps when the terminal grant is taken away. `control`
    /// is the only permission that carries the terminal, so removing it means
    /// dropping to the highest permission that does not.
    var permissionWithoutTerminal: DevicePermission {
        permission == .control ? .interact : permission
    }
}

/// What an open pairing sheet is waiting for.
///
/// This is modeled rather than shown as one string because the endings differ:
/// a code with no address is a settings problem, a code nobody scanned simply
/// expires, and a phone that enrolled has a phrase the person must check.
enum RemotePairingProgress: Equatable, Sendable {
    /// No sheet, or nothing to report yet.
    case idle
    /// The code is displayed and registered; waiting for a phone.
    case waiting
    /// The code carries no control-plane address, because none is configured.
    case unaddressed
    /// A phone enrolled and was recorded on this Mac.
    case enrolled(name: String, phrase: String?)
    /// The phone enrolled with the control plane but could not be recorded.
    case failed(String)
}

/// The answer to `latch remote-access pair confirm`. It is a device row plus
/// the short authentication phrase for the pairing that created it, which is
/// the value the person compares against the phone's screen.
struct RemotePairingConfirmation: Codable, Equatable, Sendable {
    let deviceID: String
    let name: String
    let permission: DevicePermission
    let revoked: Bool
    let pairingPhrase: String?

    enum CodingKeys: String, CodingKey {
        case name, permission, revoked, pairingPhrase
        case deviceID = "deviceId"
    }
}

/// One-time material a phone scans. The secret exists only in this value and is
/// never persisted by the app.
struct PairingMaterial: Codable, Equatable, Identifiable, Sendable {
    var id: String { pairingID }

    let formatVersion: Int
    let pairingID: String
    let secret: String
    let macPublicKey: String
    let expiresAt: UInt64
    /// Where the phone enrolls. The CLI does not know this — it has no HTTP
    /// client — so it is attached here once the desktop app has registered the
    /// code with the control plane it is configured for. Absent on a Mac that
    /// pairs over the local network only, in which case the phone has to be
    /// given an address by hand.
    let controlPlane: String?
    /// What this Mac calls itself, for the phone's confirmation screen.
    /// Advisory: the pinned key, not the name, is what pairing moves.
    let macName: String?

    enum CodingKeys: String, CodingKey {
        case formatVersion, secret, macPublicKey, expiresAt, controlPlane, macName
        case pairingID = "pairingId"
    }

    var expiryDate: Date { Date(timeIntervalSince1970: TimeInterval(expiresAt)) }

    /// Whether a phone that scans this can tell where to enroll.
    var carriesAddress: Bool { !(controlPlane ?? "").isEmpty }

    /// The same one-time material, told where to enroll.
    func addressed(to controlPlane: URL, macName: String?) -> PairingMaterial {
        let trimmed = macName?.trimmingCharacters(in: .whitespacesAndNewlines)
        return PairingMaterial(
            formatVersion: formatVersion,
            pairingID: pairingID,
            secret: secret,
            macPublicKey: macPublicKey,
            expiresAt: expiresAt,
            controlPlane: controlPlane.absoluteString,
            macName: (trimmed?.isEmpty ?? true) ? nil : trimmed
        )
    }

    /// The exact document a phone consumes, as JSON with the same camelCase
    /// keys `latch remote-access pair create --json` emits. The phone parses
    /// this shape, so re-encoding rather than inventing a compact format keeps
    /// the two sides on one contract.
    ///
    /// It carries the one-time secret, the Mac's public identity to pin, and
    /// the public control-plane address to enroll against — no Mac private key,
    /// no gateway address, no bearer token, and no session data.
    func pairingDocument() throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return String(decoding: try encoder.encode(self), as: UTF8.self)
    }
}

/// A row of the bounded local audit trail. The CLI writes only these coarse
/// fields — no names, addresses, keys, or application content.
struct RemoteAuditEvent: Codable, Identifiable, Equatable, Sendable {
    var id: String { "\(timestamp)-\(event)-\(deviceID ?? "")-\(result)" }
    let timestamp: UInt64
    let event: String
    let deviceID: String?
    let result: String

    enum CodingKeys: String, CodingKey {
        case timestamp, event, result
        case deviceID = "deviceId"
    }

    var date: Date { Date(timeIntervalSince1970: TimeInterval(timestamp)) }

    var isSecurityRelevant: Bool {
        RemoteAuditEvent.securityEvents.contains(event) || result != "ok"
    }

    private static let securityEvents: Set<String> = [
        "remote_access_enabled",
        "remote_access_disabled",
        "pairing_created",
        "pairing_confirmed",
        "device_revoked",
        "device_key_rotated",
        "permission_granted",
        "relay_enabled",
        "relay_disabled",
        "connection_rejected",
    ]

    var summary: String {
        event.replacingOccurrences(of: "_", with: " ").capitalized
    }
}

struct RemoteDiagnostics: Codable, Equatable, Sendable {
    let formatVersion: Int
    let remoteAccessEnabled: Bool
    let relayEnabled: Bool
    let pairedDevices: Int
    let revokedDevices: Int
    let eventCounts: [String: UInt64]
    /// How the connections this Mac served were routed. Modelled here rather
    /// than ignored because the app re-encodes the bundle before writing it:
    /// a field it does not decode is a field the exported file silently loses,
    /// and these counters are the point of exporting one during a field run.
    ///
    /// Optional so a bundle from an older `latch` still decodes. A missing
    /// block means the CLI did not measure, which is not the same claim as
    /// zero relayed connections.
    let pathSelection: RemotePathSelection?
}

/// Non-content path counters from the bounded local audit trail.
struct RemotePathSelection: Codable, Equatable, Sendable {
    let routes: [String: UInt64]
    let connections: UInt64
    let direct: UInt64
    let relay: UInt64
    let iceAnswers: UInt64
    let iceAnswersConnected: UInt64

    /// Share of served connections that were relayed, `nil` before any.
    ///
    /// No connections is not zero percent relayed, and a release gate reading
    /// "0%" must not be able to come from an empty counter.
    var relayShare: Double? {
        guard connections > 0 else { return nil }
        return Double(relay) / Double(connections)
    }
}

/// The supervised lifecycle the user drives from Settings.
enum RemoteAccessPhase: Equatable, Sendable {
    /// Remote access is off; no identity is used and no helper runs.
    case off
    /// Remote access is on and the helper is starting or restarting.
    case starting
    /// The helper is running and advertising an authenticated LAN listener.
    case online(listener: String)
    /// Remote access is on but the helper could not be kept running.
    case failed(String)

    var isRunning: Bool {
        if case .online = self { return true }
        return false
    }

    var label: String {
        switch self {
        case .off: return "Off"
        case .starting: return "Starting…"
        case .online: return "Online"
        case .failed: return "Stopped"
        }
    }
}
