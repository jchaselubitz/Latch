import Foundation

enum SessionState: String, Codable, CaseIterable, Sendable {
    case running
    case exited
    case lost

    var isLive: Bool { self == .running }
    var isAttachable: Bool { self != .lost }
}

struct SessionSummary: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let title: String?
    let state: SessionState
    let cwd: String
    let commandLabel: String
    let createdAt: String
    let lastActivityAt: String?
    let idleMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case id, name, title, state, cwd
        case commandLabel = "command_label"
        case createdAt = "created_at"
        case lastActivityAt = "last_activity_at"
        case idleMs = "idle_ms"
    }
}

struct ListReport: Codable, Sendable {
    let sessions: [SessionSummary]
}

struct TerminalSize: Codable, Hashable, Sendable {
    let cols: UInt16
    let rows: UInt16
}

struct ExitRecord: Codable, Hashable, Sendable {
    let code: Int32?
    let signal: String?
    let exitedAt: String

    enum CodingKeys: String, CodingKey {
        case code, signal
        case exitedAt = "exited_at"
    }
}

struct InspectReport: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let title: String?
    let state: SessionState
    let cwd: String
    let commandLabel: String
    let createdAt: String
    let initialSize: TerminalSize
    let size: TerminalSize?
    let exit: ExitRecord?
    let attached: UInt?

    enum CodingKeys: String, CodingKey {
        case id, name, title, state, cwd, size, exit, attached
        case commandLabel = "command_label"
        case createdAt = "created_at"
        case initialSize = "initial_size"
    }
}

struct CreateReport: Codable, Sendable {
    let protocolVersion: UInt32
    let session: CreatedSession
}

struct CreatedSession: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let state: SessionState
    let createdAt: String
}

struct StopReport: Codable, Sendable {
    let id: String
    let state: SessionState
    let stopped: Bool
}
struct StopAllReport: Codable, Sendable { let sessions: [StopReport] }
struct RenameReport: Codable, Sendable { let id: String; let name: String }
struct RemoveReport: Codable, Sendable { let id: String; let removed: Bool }

struct ResizeReport: Codable, Sendable {
    let id: String
    let cols: UInt16
    let rows: UInt16
    let pinned: Bool
}

struct RetainedSession: Codable, Identifiable, Sendable {
    var id: String { sessionID }
    let sessionID: String
    let reclaimableInMs: UInt64

    enum CodingKeys: String, CodingKey {
        case sessionID = "id"
        case reclaimableInMs = "reclaimable_in_ms"
    }
}

struct PruneReport: Codable, Sendable {
    let reclaimed: [String]
    let retained: [RetainedSession]
    let dryRun: Bool

    enum CodingKeys: String, CodingKey {
        case reclaimed, retained
        case dryRun = "dry_run"
    }
}

struct CapabilitiesReport: Codable, Sendable {
    let protocolVersion: UInt32
    let productVersion: String
    let capabilities: CapabilityFlags
}

struct CapabilityFlags: Codable, Sendable {
    let create: Bool
    let openViewer: Bool
    let localAttach: Bool
    let cloudAttach: Bool
    let selfUpdate: Bool
    let extensions: [String]
}

struct DoctorReport: Codable, Sendable {
    let tmuxVersion: String?
    let findings: [DoctorFinding]
}
struct DoctorFinding: Codable, Identifiable, Sendable {
    var id: String { code }
    let code: String
    let severity: String
    let message: String
}

enum CLIUpdateStatus: String, Codable, Sendable {
    case current
    case available
    case installed
}

struct CLIUpdateReport: Codable, Sendable {
    let status: CLIUpdateStatus
    let currentVersion: String
    let latestVersion: String?
    let releaseURL: URL?
    let installedPath: String?

    enum CodingKeys: String, CodingKey {
        case status
        case currentVersion = "current_version"
        case latestVersion = "latest_version"
        case releaseURL = "release_url"
        case installedPath = "installed_path"
    }
}

struct NewSessionRequest: Sendable {
    var name = ""
    var title = ""
    var cwd = FileManager.default.homeDirectoryForCurrentUser.path
    var command = ""
    var cols: UInt16 = 120
    var rows: UInt16 = 36
}

struct ResizeSessionRequest: Sendable {
    var cols: UInt16
    var rows: UInt16
    var pin = false
}

struct LaunchManifest: Encodable, Sendable {
    let formatVersion = 1
    let launch: Launch
    let display: Display

    struct Launch: Encodable, Sendable {
        let argv: [String]
        let cwd: String
        let env: [String: String] = [:]
        let inheritEnv = true
        let size: TerminalSize
        let term = "xterm-256color"

        enum CodingKeys: String, CodingKey {
            case argv, cwd, env, size, term
            case inheritEnv = "inherit_env"
        }
    }

    struct Display: Encodable, Sendable {
        let name: String?
        let title: String?
        let commandLabel: String?
        let source = Source()

        enum CodingKeys: String, CodingKey {
            case name, title, source
            case commandLabel = "command_label"
        }
    }

    struct Source: Encodable, Sendable { let kind = "desktop" }

    enum CodingKeys: String, CodingKey {
        case formatVersion = "format_version"
        case launch, display
    }
}
