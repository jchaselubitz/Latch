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
        case .interact: return "Interact"
        case .control: return "Control"
        }
    }

    var detail: String {
        switch self {
        case .observe:
            return "Read sessions, conversations, and events."
        case .interact:
            return "Also send messages and resolve prompts."
        case .control:
            return "Also open the terminal, which takes it from whatever is showing it."
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

    enum CodingKeys: String, CodingKey {
        case formatVersion, enabled, relayEnabled, publicKey, keyGeneration
        case pairedDevices, revokedDevices, listenerAddress
        case deviceID = "deviceId"
    }

    static let unavailable = RemoteAccessStatus(
        formatVersion: 1,
        enabled: false,
        relayEnabled: false,
        deviceID: nil,
        publicKey: nil,
        keyGeneration: nil,
        pairedDevices: 0,
        revokedDevices: 0,
        listenerAddress: nil
    )
}

struct RemoteDevice: Codable, Identifiable, Equatable, Sendable {
    var id: String { deviceID }
    let deviceID: String
    let name: String
    let permission: DevicePermission
    let revoked: Bool

    enum CodingKeys: String, CodingKey {
        case name, permission, revoked
        case deviceID = "deviceId"
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
