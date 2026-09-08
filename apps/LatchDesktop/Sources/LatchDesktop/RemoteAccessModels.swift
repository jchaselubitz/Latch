import Foundation

/// The grant the Mac enforces before every proxied action.
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
        case .observe: return "Read sessions, conversations, and events."
        case .interact: return "Also send messages and resolve prompts."
        case .control: return "Control sessions and open the terminal."
        }
    }

    func permits(_ required: DevicePermission) -> Bool { rank >= required.rank }

    private var rank: Int {
        switch self {
        case .observe: return 0
        case .interact: return 1
        case .control: return 2
        }
    }
}

/// Content-free owner status from `latch remote-access status --json`.
struct RemoteAccessStatus: Codable, Equatable, Sendable {
    let formatVersion: Int
    let enabled: Bool
    let deviceID: String?
    let publicKey: String?
    let keyGeneration: UInt64?
    let pairedDevices: Int
    let revokedDevices: Int

    enum CodingKeys: String, CodingKey {
        case formatVersion, enabled, publicKey, keyGeneration, pairedDevices, revokedDevices
        case deviceID = "deviceId"
    }

    static let unavailable = RemoteAccessStatus(
        formatVersion: 1,
        enabled: false,
        deviceID: nil,
        publicKey: nil,
        keyGeneration: nil,
        pairedDevices: 0,
        revokedDevices: 0
    )
}

struct RemoteDevice: Codable, Identifiable, Equatable, Sendable {
    var id: String { deviceID }
    let deviceID: String
    let name: String
    let permission: DevicePermission
    let revoked: Bool
    let grantRevision: UInt64
    let controlPlaneDeviceID: String?

    enum CodingKeys: String, CodingKey {
        case name, permission, revoked, grantRevision
        case deviceID = "deviceId"
        case controlPlaneDeviceID = "controlPlaneDeviceId"
    }

    var allowsTerminal: Bool { permission == .control }
    var permissionWithoutTerminal: DevicePermission {
        permission == .control ? .interact : permission
    }
}

enum RemotePairingProgress: Equatable, Sendable {
    case idle
    case waiting
    case comparing(name: String, permission: DevicePermission, code: String)
    case enrolled(name: String)
    case failed(String)
}

/// One-use enrollment document displayed as a QR code. The host admission is
/// kept out of the QR and inherited only by the Mac helper.
struct RemoteEnrollmentMaterial: Identifiable, Equatable, Sendable {
    var id: String { enrollmentID }

    let enrollmentID: String
    let enrollmentSecret: String
    let hostPublicKey: String
    let admissionCode: String
    let hostAdmission: String
    let relayURL: String
    let expiresAt: UInt64
    let controlPlane: String
    let macName: String

    var expiryDate: Date { Date(timeIntervalSince1970: TimeInterval(expiresAt)) }

    func pairingDocument() throws -> String {
        struct QRDocument: Encodable {
            let version = 1
            let controlPlane: String
            let enrollmentId: String
            let hostPublicKey: String
            let admissionCode: String
            let enrollmentSecret: String
            let expiresAt: UInt64
            let macName: String
        }
        let document = QRDocument(
            controlPlane: controlPlane,
            enrollmentId: enrollmentID,
            hostPublicKey: hostPublicKey,
            admissionCode: admissionCode,
            enrollmentSecret: enrollmentSecret,
            expiresAt: expiresAt,
            macName: macName
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return String(decoding: try encoder.encode(document), as: UTF8.self)
    }
}

struct RemoteEnrollmentPending: Decodable, Equatable, Sendable {
    let type: String
    let version: Int
    let enrollmentId: String
    let provisionalDeviceId: String
    let controllerPublicKey: String
    let name: String
    let permission: DevicePermission
    let comparison: String
}

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
    var isSecurityRelevant: Bool { Self.securityEvents.contains(event) || result != "ok" }
    private static let securityEvents: Set<String> = [
        "remote_access_enabled", "remote_access_disabled", "enrollment_authorized",
        "device_revoked", "device_key_rotated", "permission_granted", "connection_rejected",
    ]
    var summary: String { event.replacingOccurrences(of: "_", with: " ").capitalized }
}

/// One spooled attention event from `latch remote-access attention-events`.
struct RemoteAttentionEvent: Codable, Equatable, Identifiable, Sendable {
    var id: String { eventID }
    let eventID: String
    let deviceID: String
    let kind: String
    let createdAt: UInt64

    enum CodingKeys: String, CodingKey {
        case kind, createdAt
        case eventID = "eventId"
        case deviceID = "deviceId"
    }
}

/// Where one helper's link is, as the helper reports it.
enum HelperLinkStatus: String, Equatable, Sendable {
    case lanReady = "lan_ready"
    case connecting
    case waitingForPeer = "waiting_for_peer"
    case authenticating
    case ready
    case linkClosed = "link_closed"
    case offline

    var isConnected: Bool { self == .ready }
}

enum RemoteAccessPhase: Equatable, Sendable {
    case off
    case starting
    case onlineRelay(peers: Int)
    case failed(String)

    var isRunning: Bool {
        if case .onlineRelay = self { return true }
        return false
    }

    var label: String {
        switch self {
        case .off: return "Off"
        case .starting: return "Starting…"
        case .onlineRelay: return "Online"
        case .failed: return "Stopped"
        }
    }
}
