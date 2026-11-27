import CryptoKit
import Foundation

/// The minimal reference saved before a scoped Git submission. The review,
/// Git payload and credentials remain out of the recovery file.
public struct PendingGitWrite: Codable, Equatable, Sendable {
    public let requestID: UUID
    public let root: String
    public let operation: GitMutationKind

    fileprivate let pairing: String
    fileprivate let worktreeRef: String
}

public enum ScopedGitWriteError: Error, Equatable, Sendable {
    case invalidRequest
    case alreadyPending
    case unknownPendingWrite
    case storageUnavailable
    case receiptMismatch
}

/// A bounded, owner-authorized interaction for exact-worktree Git writes.
/// Use the same interaction's worktrees to review the checkout and prepare
/// the action. The caller checks its own project/resource write authority
/// immediately before calling submit.
@MainActor
public final class ScopedGitWriteInteraction {
    public let worktrees: ScopedGitReadInteraction

    private let access: ScopedReadAuthorization
    private let pairing: String
    private let store: PendingGitWriteStore

    init(access: ScopedReadAuthorization, pairing: PairedDeviceRecord, store: PendingGitWriteStore) {
        self.access = access
        self.store = store
        self.worktrees = ScopedGitReadInteraction(access: access)
        let identity = [
            pairing.deviceId, pairing.devicePublicKey, pairing.mac.deviceId, pairing.mac.publicKey,
            String(pairing.pairedAt.timeIntervalSinceReferenceDate)
        ].joined(separator: "\u{0}")
        self.pairing = SHA256.hash(data: Data(identity.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    public func invalidate() {
        worktrees.invalidate()
    }

    /// The current pairing's outstanding requests, including results the
    /// caller has not yet acknowledged. Available in a new interaction
    /// after cold launch, including if the gateway restarted.
    public func pendingWrites() async throws -> [PendingGitWrite] {
        try await authorized { _ in try store.load().filter { $0.pairing == pairing } }
    }

    public func submit(_ action: GitMutationAction, for worktree: ScopedGitWorktree) async throws -> GitMutationReceipt {
        let reference = try worktrees.reference(for: worktree)
        guard let requestID = UUID(uuidString: action.requestId), action.path == worktree.repository.root else {
            throw ScopedGitWriteError.invalidRequest
        }
        let pending = PendingGitWrite(
            requestID: requestID, root: worktree.repository.root, operation: action.kind,
            pairing: pairing, worktreeRef: reference
        )
        return try await authorized { gateway in
            do {
                let receipt = try await gateway.gitMutation(action, worktreeRef: reference) {
                    _ = try self.access.currentGateway(for: .gitWorktreeWrites)
                    try self.store.insert(pending)
                }
                return try validated(receipt, for: pending)
            } catch let error as LatchError {
                if case .git(let code, _) = error,
                   code == "invalid_request" || code == "worktree_busy" {
                    // These responses are issued before this request is accepted.
                    try store.remove(pending)
                }
                throw error
            }
        }
    }

    /// Reads only the original receipt. A nil result means the gateway has
    /// no receipt yet. Keep the local reference and never resend from here.
    public func recover(_ pending: PendingGitWrite) async throws -> GitMutationReceipt? {
        try await authorized { gateway in
            try requireSaved(pending)
            do {
                let receipt = try await gateway.gitReceipt(
                    requestID: pending.requestID.uuidString, worktreeRef: pending.worktreeRef
                )
                return try validated(receipt, for: pending)
            } catch LatchError.git(code: "receipt_not_found", reason: _) {
                return nil
            }
        }
    }

    /// Checks an uncertain push against its original remote. The gateway
    /// uses the stored request and never repeats the push.
    public func checkRemote(_ pending: PendingGitWrite) async throws -> GitPushReconciliation {
        try await authorized { gateway in
            try requireSaved(pending)
            let result = try await gateway.gitPushReconcile(
                requestId: pending.requestID.uuidString, worktreeRef: pending.worktreeRef
            )
            _ = try validated(result.receipt, for: pending)
            return result
        }
    }

    /// Forget a completed or rejected action after presenting the outcome.
    /// Missing, pending and uncertain results stay available for recovery.
    @discardableResult
    public func acknowledge(_ pending: PendingGitWrite) async throws -> Bool {
        guard let receipt = try await recover(pending),
              receipt.state == .completed || receipt.state == .rejected else { return false }
        try store.remove(pending)
        return true
    }

    private func requireSaved(_ pending: PendingGitWrite) throws {
        guard pending.pairing == pairing, try store.load().contains(pending) else {
            throw ScopedGitWriteError.unknownPendingWrite
        }
    }

    private func validated(_ receipt: GitMutationReceipt, for pending: PendingGitWrite) throws -> GitMutationReceipt {
        guard UUID(uuidString: receipt.requestId) == pending.requestID,
              receipt.root == pending.root, receipt.operation == pending.operation else {
            throw ScopedGitWriteError.receiptMismatch
        }
        return receipt
    }

    private func authorized<Value>(_ operation: (LatchGateway) async throws -> Value) async throws -> Value {
        do {
            return try await access.read(.gitWorktreeWrites, operation: operation)
        } catch let error as ScopedReadAccessError {
            throw ScopedGitReadError(accessError: error)
        } catch LatchError.unauthorized {
            access.revokeAccess()
            throw ScopedGitReadError.accessDenied
        } catch LatchError.git(code: "worktree_access_denied", reason: _) {
            access.revokeAccess()
            throw ScopedGitReadError.accessDenied
        }
    }
}

@MainActor
struct PendingGitWriteStore {
    let file: URL

    static var application: Self {
        let folder = URL.applicationSupportDirectory.appending(path: "LatchGitRecovery", directoryHint: .isDirectory)
        return Self(file: folder.appending(path: "pending-writes.json"))
    }

    func load() throws -> [PendingGitWrite] {
        do {
            return try JSONDecoder().decode([PendingGitWrite].self, from: Data(contentsOf: file))
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return []
        } catch {
            throw ScopedGitWriteError.storageUnavailable
        }
    }

    func insert(_ pending: PendingGitWrite) throws {
        let entries = try load()
        guard !entries.contains(where: { $0.pairing == pending.pairing && $0.requestID == pending.requestID }) else {
            throw ScopedGitWriteError.alreadyPending
        }
        try save(entries + [pending])
    }

    func remove(_ pending: PendingGitWrite) throws {
        try save(load().filter { $0 != pending })
    }

    private func save(_ entries: [PendingGitWrite]) throws {
        do {
            var folder = file.deletingLastPathComponent()
            try FileManager.default.createDirectory(
                at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700]
            )
            var properties = URLResourceValues()
            properties.isExcludedFromBackup = true
            try folder.setResourceValues(properties)
            #if os(iOS)
            let options: Data.WritingOptions = [.atomic, .completeFileProtection]
            #else
            let options: Data.WritingOptions = .atomic
            #endif
            try JSONEncoder().encode(entries).write(to: file, options: options)
        } catch {
            throw ScopedGitWriteError.storageUnavailable
        }
    }
}
