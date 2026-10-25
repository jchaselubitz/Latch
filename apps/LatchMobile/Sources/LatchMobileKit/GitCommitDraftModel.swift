import Foundation
import Observation

public struct GitCommitDraftError: Error, Sendable {
    public let failure: GitCommitMessageFailure

    public init(failure: GitCommitMessageFailure) { self.failure = failure }

    public var message: String {
        switch failure.code {
        case .unsupported: return "Update Latch on your Mac to generate drafts. You can still write a message."
        case .unconfigured: return "Set a Gemini key on your Mac with latch commit-message set-key, then try again."
        case .unauthorized: return "This phone needs a current Control grant on your Mac."
        case .noStagedChanges: return "Stage changes before generating a draft."
        case .staleSource: return "The branch, HEAD or staged changes changed. Refresh, review the staged files and generate again."
        case .unusableContext, .contextTooLarge: return "These staged changes cannot be safely included in a draft. Write a message manually."
        case .providerRefusal: return "Google declined to generate a draft. Try again or write a message."
        case .rateLimited: return "Drafting is busy or rate limited. Try again in \(failure.retryAfterSeconds ?? 60) seconds."
        case .timedOut: return "Draft generation timed out. Try again or write a message."
        case .cancelled: return "Generation cancelled. Source already sent to Google cannot be recalled."
        case .providerError: return "Google could not generate a draft. Check the Gemini key and model on your Mac, or write a message."
        case .gitUnavailable, .invalidRequest: return "Your Mac could not read these staged changes for a draft. Refresh or write a message."
        }
    }
}

/// The identity shown in the sheet, independent of unstaged worktree edits.
public struct GitCommitDraftIdentity: Equatable, Sendable {
    public let root: String
    public let expected: GitDraftExpected

    public init(status: GitStatus) {
        root = status.root
        expected = GitDraftExpected(
            branch: status.preconditions?.branch, head: status.preconditions?.head,
            index: status.preconditions?.index ?? ""
        )
    }
}

/// Owned by one presentation of CommitSheet, never cached with the repository.
/// Cancellation invalidates the intent before touching the network. A transport
/// ignoring cancellation still cannot write into this sheet or a later request.
@MainActor
@Observable
public final class GitCommitDraftModel {
    public typealias Generate = (GitCommitMessageRequest) async throws -> GitCommitMessageResponse
    public typealias Cancel = (String) async -> Void

    public private(set) var summary = ""
    public private(set) var details = ""
    public private(set) var isGenerating = false
    public private(set) var notice: String?
    public private(set) var result: GitCommitMessageResponse?
    public private(set) var isStale = false
    private var revision: UInt64 = 0
    private var requestID: String?
    private var task: Task<Void, Never>?
    private var active = true
    private var reviewedIdentity: GitCommitDraftIdentity?
    private let repository: GitRepositoryModel
    private let generate: Generate
    private let cancel: Cancel
    private let supported: () -> Bool
    private let readiness: () -> GitDraftReadinessState?

    public init(
        repository: GitRepositoryModel, supported: @escaping () -> Bool,
        readiness: @escaping () -> GitDraftReadinessState?,
        generate: @escaping Generate, cancel: @escaping Cancel
    ) {
        self.repository = repository
        self.supported = supported
        self.readiness = readiness
        self.generate = generate
        self.cancel = cancel
    }

    public var identity: GitCommitDraftIdentity? { repository.status.map(GitCommitDraftIdentity.init) }
    public var hasSupport: Bool { supported() }
    public var availabilityNote: String? {
        guard hasSupport else { return "AI drafts need a newer Latch gateway on your Mac. Manual messages are available." }
        guard repository.isOnline() else { return "Reconnect to your Mac to generate a draft." }
        guard repository.hasAccess() else { return GitAccessError.unavailable.message }
        switch readiness() {
        case .unconfigured: return "Set a Gemini key on your Mac with latch commit-message set-key, then reconnect to refresh readiness."
        case .unavailable: return "Your Mac cannot access its Gemini configuration. Check the selected Latch CLI and Keychain on your Mac."
        case nil: return "Your Mac has not reported draft readiness. Reconnect or write a message manually."
        case .ready: return repository.commitBlockReason
        }
    }
    public var canGenerate: Bool { active && !isGenerating && availabilityNote == nil && repository.canCommit }
    public var requiresReview: Bool {
        guard result != nil else { return false }
        return isStale || reviewedIdentity != identity
    }
    public var canCommitMessage: Bool { active && !isGenerating && !requiresReview }
    public var coverageNote: String? {
        guard let coverage = result?.coverage,
              !coverage.omissions.isEmpty || coverage.patchFiles < coverage.totalFiles
        else { return nil }
        return "Partial context: Google received patches for \(coverage.patchFiles) of \(coverage.totalFiles) staged files. Some content was omitted or represented only by metadata. Review the message against all staged changes."
    }

    public func editSummary(_ text: String) { summary = text; revision &+= 1 }
    public func editDetails(_ text: String) { details = text; revision &+= 1 }

    public func sourceDidChange() {
        if result != nil, reviewedIdentity != identity { isStale = true }
    }

    /// Explicitly keeps the current editable text after the person reviews the
    /// refreshed staged files. The original result/source is retained unchanged.
    public func acknowledgeManualReview() {
        guard repository.canCommit, !isGenerating, result != nil else { return }
        reviewedIdentity = identity
        isStale = false
    }

    /// Called only by the explicit Generate action, after the UI's disclosure.
    public func startGeneration() {
        guard canGenerate, let identity, !identity.expected.index.isEmpty else { return }
        let id = UUID().uuidString.lowercased()
        let textRevision = revision
        requestID = id
        isGenerating = true
        notice = nil
        let request = GitCommitMessageRequest(requestId: id, path: identity.root, expected: identity.expected)
        task = Task { [weak self] in
            guard let self else { return }
            do {
                let response = try await generate(request)
                guard active, requestID == id, !Task.isCancelled else { return }
                // Read again to catch changes after the gateway's final source
                // check. Failure keeps the draft out of the editable fields.
                guard !repository.isLoading else {
                    notice = "The repository is refreshing. Review the staged files and generate again."
                    finish(id)
                    return
                }
                await repository.refresh()
                guard active, requestID == id, !Task.isCancelled else { return }
                guard repository.hasAccess(), !repository.isStale,
                      self.identity == identity, response.requestId == id,
                      response.source.root == identity.root,
                      response.source.branch == identity.expected.branch,
                      response.source.head == identity.expected.head,
                      response.source.index == identity.expected.index else {
                    notice = "The repository changed or could not be refreshed. Review the staged files and generate again."
                    finish(id)
                    return
                }
                guard revision == textRevision else {
                    notice = "Your newer typing was kept. Generate again when you are ready to replace the message."
                    finish(id)
                    return
                }
                guard !response.summary.isEmpty, response.summary.count <= 72,
                      !response.summary.contains("\n"), !response.summary.contains("\r") else {
                    notice = "Google returned an invalid summary. Write a message or generate again."
                    finish(id)
                    return
                }
                summary = response.summary
                details = response.body ?? ""
                result = response
                reviewedIdentity = identity
                isStale = false
                notice = "Draft ready. Review and edit the message before committing."
                finish(id)
            } catch {
                guard active, requestID == id, !Task.isCancelled else { return }
                if let error = error as? GitCommitDraftError {
                    notice = error.message
                    if error.failure.code == .staleSource {
                        if result != nil { isStale = true }
                        await repository.refresh()
                    }
                } else if error is GitAccessError {
                    notice = GitAccessError.unavailable.message
                } else {
                    notice = "Could not generate a draft. Check the connection and Control grant on your Mac, or write a message manually."
                }
                finish(id)
            }
        }
    }

    public func cancelGeneration() {
        guard let id = requestID else { return }
        requestID = nil
        task?.cancel()
        task = nil
        isGenerating = false
        notice = "Generation cancelled. Source already sent to Google cannot be recalled."
        let cancel = cancel
        Task { await cancel(id) }
    }

    public func dismiss() {
        active = false
        cancelGeneration()
    }

    private func finish(_ id: String) {
        guard requestID == id else { return }
        requestID = nil
        isGenerating = false
        task = nil
    }
}
