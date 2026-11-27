import Foundation
import Observation

public enum SharedFilesReadError: Error, Equatable, Sendable {
    case unsupported
    case ownerCancelled
    case ownerAuthenticationUnavailable
    case ownerAuthorizationExpired
    case accessDenied
    case transportUnavailable
    case invalidated
    case rootUnavailable
    case invalidPath
    case entryUnavailable
    case entryChanged
    case staleCursor
    case directoryTooLarge
    case gateway(LatchError)

    init(accessError: ScopedReadAccessError) {
        switch accessError {
        case .ownerCancelled: self = .ownerCancelled
        case .ownerAuthenticationUnavailable: self = .ownerAuthenticationUnavailable
        case .ownerAuthorizationExpired: self = .ownerAuthorizationExpired
        case .accessDenied: self = .accessDenied
        case .transportUnavailable: self = .transportUnavailable
        case .invalidated: self = .invalidated
        case .unsupported: self = .unsupported
        }
    }

    init?(wireCode: String) {
        switch wireCode {
        case "shared_root_unavailable": self = .rootUnavailable
        case "invalid_shared_path": self = .invalidPath
        case "shared_entry_unavailable": self = .entryUnavailable
        case "shared_entry_changed": self = .entryChanged
        case "stale_cursor": self = .staleCursor
        case "shared_directory_too_large": self = .directoryTooLarge
        default: return nil
        }
    }
}

extension AppModel {
    public var advertisesSharedFiles: Bool {
        GatewayCompatibility.supports(endpoint: .sharedFiles, capabilities: linkState.cachedCapabilities)
    }

    /// Starts one owner-authorized read interaction on the authenticated host.
    public func beginSharedFilesRead() async throws -> SharedFilesReadInteraction {
        do {
            let access = try await authorizeScopedRead(endpoint: .sharedFiles, reason: "Read shared files on your Mac.")
            return SharedFilesReadInteraction(access: access)
        } catch let error as ScopedReadAccessError {
            throw SharedFilesReadError(accessError: error)
        }
    }
}

/// Bounded reads under explicit host shares. Keep returned content in memory
/// and discard it when the project binding or this interaction becomes invalid.
@MainActor
@Observable
public final class SharedFilesReadInteraction {
    private let access: ScopedReadAuthorization

    init(access: ScopedReadAuthorization) {
        self.access = access
    }

    public var isValid: Bool {
        (try? access.currentGateway(for: .sharedFiles)) != nil
    }

    public func invalidate() {
        access.invalidate()
    }

    public func roots() async throws -> [SharedRoot] {
        try await read { try await $0.sharedRoots().roots }
    }

    public func directory(
        rootID: String, path: String = "", cursor: String? = nil
    ) async throws -> SharedDirectoryPage {
        try await read { try await $0.sharedDirectory(rootID: rootID, path: path, cursor: cursor) }
    }

    public func preview(rootID: String, path: String) async throws -> SharedFilePreview {
        try await read { try await $0.sharedFilePreview(rootID: rootID, path: path) }
    }

    private func read<Value>(
        _ operation: (LatchGateway) async throws -> Value
    ) async throws -> Value {
        do {
            return try await access.read(.sharedFiles, operation: operation)
        } catch let error as ScopedReadAccessError {
            throw SharedFilesReadError(accessError: error)
        } catch let error as LatchError {
            switch error {
            case .unauthorized:
                access.revokeAccess()
                throw SharedFilesReadError.accessDenied
            case .endpointUnavailable:
                throw SharedFilesReadError.unsupported
            case .transport:
                throw SharedFilesReadError.transportUnavailable
            default:
                throw SharedFilesReadError.gateway(error)
            }
        }
    }
}
