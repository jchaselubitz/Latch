import Foundation
import Observation

public enum ScopedGitReadError: Error, Equatable, Sendable {
    case ownerCancelled
    case ownerAuthenticationUnavailable
    case unsupported(GatewayEndpointsName)
    case accessDenied
    case transportUnavailable
    case invalidated
    case ownerAuthorizationExpired
    case invalidWorktree
    case invalidSummaryCount
    case staleWorktree
    case staleCursor
    case gateway(LatchError)

    init(accessError: ScopedReadAccessError) {
        switch accessError {
        case .ownerCancelled: self = .ownerCancelled
        case .ownerAuthenticationUnavailable: self = .ownerAuthenticationUnavailable
        case .ownerAuthorizationExpired: self = .ownerAuthorizationExpired
        case .accessDenied: self = .accessDenied
        case .transportUnavailable: self = .transportUnavailable
        case .invalidated: self = .invalidated
        case .unsupported(let endpoint): self = .unsupported(endpoint)
        }
    }
}

/// An exact worktree opened during one authorized interaction. The reference
/// stays opaque to the caller and cannot be submitted in another interaction.
public struct ScopedGitWorktree: Sendable {
    public let repository: GitRepository
    fileprivate let reference: String
    fileprivate let interactionID: UUID
}

/// Read-only Git access on the authenticated paired Mac. All operations use
/// the owner check from the start of the interaction, up to its original
/// expiration. After a link change, begin a new interaction and reopen roots.
///
/// This object stores no results. Observe `isValid` when displaying returned
/// status or diff pages, and drop those values on invalidation. Call
/// `invalidate()` when the caller's own account, project or resource access
/// changes, or when the user leaves the interaction.
@MainActor
@Observable
public final class ScopedGitReadInteraction {
    public static let maximumSummaryCount = 8

    private let access: ScopedReadAuthorization
    private let id = UUID()

    init(access: ScopedReadAuthorization) {
        self.access = access
    }

    public var isValid: Bool {
        (try? access.currentGateway(for: .gitWorktreeReads)) != nil
    }

    public func invalidate() {
        access.invalidate()
    }

    public func open(path: String) async throws -> ScopedGitWorktree {
        let result = try await read(.gitWorktreeReads) { try await $0.openGitWorktree(path: path) }
        return ScopedGitWorktree(repository: result.repository, reference: result.worktreeRef, interactionID: id)
    }

    /// Returns one typed outcome per input in the same order, including
    /// when individual worktrees are unavailable.
    public func summaries(for worktrees: [ScopedGitWorktree]) async throws -> [GitWorktreeSummaryOutcome] {
        guard (1...Self.maximumSummaryCount).contains(worktrees.count) else {
            throw ScopedGitReadError.invalidSummaryCount
        }
        let references = try worktrees.map(reference(for:))
        let result = try await read(.gitWorktreeSummaries) {
            try await $0.gitWorktreeSummaries(worktreeRefs: references)
        }
        guard result.results.count == references.count else {
            throw ScopedGitReadError.gateway(.malformedResponse("Expected one summary outcome per worktree."))
        }
        return result.results
    }

    public func status(for worktree: ScopedGitWorktree) async throws -> GitStatus {
        let reference = try reference(for: worktree)
        return try await read(.gitWorktreeReads) { try await $0.gitStatus(worktreeRef: reference) }
    }

    /// Reads one page using the path key and area from status. Continue with
    /// that page's cursor while the interaction and diff are current.
    public func diff(
        for worktree: ScopedGitWorktree, fileKey: String, area: GitChangeArea, cursor: String? = nil
    ) async throws -> GitFileDiff {
        let reference = try reference(for: worktree)
        return try await read(.gitWorktreeReads) {
            try await $0.gitDiff(worktreeRef: reference, fileKey: fileKey, area: area, cursor: cursor)
        }
    }

    public func branches(for worktree: ScopedGitWorktree) async throws -> GitBranchList {
        let reference = try reference(for: worktree)
        return try await read(.gitWorktreePreviews) { try await $0.gitBranches(worktreeRef: reference) }
    }

    public func sync(for worktree: ScopedGitWorktree, cursor: String? = nil) async throws -> GitSyncPreview {
        let reference = try reference(for: worktree)
        return try await read(.gitWorktreePreviews) { try await $0.gitSync(worktreeRef: reference, cursor: cursor) }
    }

    public func pushPreview(
        for worktree: ScopedGitWorktree, selection: GitPushSuggestion? = nil, cursor: String? = nil
    ) async throws -> GitPushPreview {
        let reference = try reference(for: worktree)
        return try await read(.gitWorktreePreviews) {
            try await $0.gitPushPreview(worktreeRef: reference, selection: selection, cursor: cursor)
        }
    }

    func reference(for worktree: ScopedGitWorktree) throws -> String {
        guard worktree.interactionID == id else { throw ScopedGitReadError.invalidWorktree }
        return worktree.reference
    }

    private func read<Value>(
        _ endpoint: GatewayEndpointsName, operation: (LatchGateway) async throws -> Value
    ) async throws -> Value {
        do {
            return try await access.read(endpoint, operation: operation)
        } catch let error as ScopedReadAccessError {
            throw ScopedGitReadError(accessError: error)
        } catch let error as LatchError {
            switch error {
            case .unauthorized, .git(code: "worktree_access_denied", reason: _):
                access.revokeAccess()
                throw ScopedGitReadError.accessDenied
            case .endpointUnavailable(let endpoint):
                throw ScopedGitReadError.unsupported(endpoint)
            case .transport:
                try Task.checkCancellation()
                throw ScopedGitReadError.transportUnavailable
            case .git(code: "stale_worktree_ref", reason: _):
                throw ScopedGitReadError.staleWorktree
            case .git(code: "stale_cursor", reason: _):
                throw ScopedGitReadError.staleCursor
            default:
                throw ScopedGitReadError.gateway(error)
            }
        }
    }
}
