import Foundation
import Observation

// MARK: - Sync sheet

/// The Sync sheet: the upstream as last fetched, the incoming commits a
/// page at a time, and the three ways to bring them in. Reading it never
/// fetches; Fetch is its own action. Every change runs through the
/// repository's one runner, so only one is ever in flight.
@MainActor
@Observable
public final class GitSyncModel {
    public typealias Reading = (String, String?) async throws -> GitSyncPreview

    public let root: String
    public private(set) var preview: GitSyncPreview?
    /// Every incoming commit read so far, newest first.
    public private(set) var incoming: [GitCommitSummary] = []
    public private(set) var isLoading = false
    public private(set) var error: String?
    /// Set when the upstream moved between pages and the list started over.
    public private(set) var notice: String?
    public var mode: GitSyncMode = .fastForward
    /// Requests this sheet sent, so it shows only their progress and outcomes.
    public private(set) var sentRequestIDs: Set<String> = []

    public let support: GitMutationSupport
    public let runner: GitMutationRunner
    private let reading: Reading
    private let blockReason: () -> String?
    private let makeRequestID: () -> String
    private let remember: (GitSyncPreview) -> Void

    init(
        root: String, runner: GitMutationRunner, support: GitMutationSupport, reading: @escaping Reading,
        blockReason: @escaping () -> String?, makeRequestID: @escaping () -> String,
        remember: @escaping (GitSyncPreview) -> Void = { _ in }
    ) {
        self.root = root
        self.runner = runner
        self.support = support
        self.reading = reading
        self.blockReason = blockReason
        self.makeRequestID = makeRequestID
        self.remember = remember
        mode = Self.defaultMode(support)
    }

    static func defaultMode(_ support: GitMutationSupport) -> GitSyncMode {
        if support.fastForward { return .fastForward }
        if support.merge { return .merge }
        return .rebase
    }

    // MARK: Reading

    public func load() async {
        await fetchPage(cursor: nil)
    }

    public var hasMore: Bool { preview?.nextCursor != nil }

    public func loadMore() async {
        guard let cursor = preview?.nextCursor else { return }
        await fetchPage(cursor: cursor)
    }

    private func fetchPage(cursor: String?) async {
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let page = try await reading(root, cursor)
            if cursor == nil {
                incoming = page.incoming
                notice = nil
            } else {
                incoming += page.incoming.filter { commit in !incoming.contains { $0.oid == commit.oid } }
            }
            preview = page
            remember(page)
            error = nil
            settleMode()
        } catch LatchError.git(code: "stale_cursor", _) {
            // The upstream moved between pages; mixing two histories would
            // misstate what comes in. Start over from the first page.
            isLoading = false
            await fetchPage(cursor: nil)
            notice = "The upstream changed on your Mac, so the incoming commits were reloaded."
        } catch {
            self.error = gitMessage(for: error)
        }
    }

    /// Keeps the chosen mode one this Mac serves.
    private func settleMode() {
        if !availableModes.contains(mode), let first = availableModes.first { mode = first }
    }

    // MARK: Modes

    /// The ways this Mac lets the phone bring commits in.
    public var availableModes: [GitSyncMode] {
        GitSyncMode.allCases.filter { mode in
            switch mode {
            case .fastForward: support.fastForward
            case .merge: support.merge
            case .rebase: support.rebase
            }
        }
    }

    /// Why `mode` can't be chosen, or nil when it can. Fast-forward is
    /// shown disabled with the Mac's sentence when the branches diverged.
    public func modeBlockedReason(_ mode: GitSyncMode) -> String? {
        guard mode == .fastForward else { return nil }
        return preview?.fastForwardBlockedReason
    }

    public func isModeEnabled(_ mode: GitSyncMode) -> Bool {
        availableModes.contains(mode) && modeBlockedReason(mode) == nil
    }

    /// After "Choose Merge or Rebase": the first of those this Mac serves.
    public func chooseMergeOrRebase() {
        if support.merge { mode = .merge } else if support.rebase { mode = .rebase }
    }

    // MARK: Update

    public var primaryTitle: String {
        let branch = preview?.head.branch ?? "HEAD"
        let upstream = preview?.upstream?.name ?? "upstream"
        return mode.buttonTitle(upstream: upstream, branch: branch)
    }

    /// Why the primary button is off, or nil when it may run.
    public var updateBlockReason: String? {
        if let reason = blockReason() { return reason }
        guard let preview else { return isLoading ? "Reading the upstream…" : "Latch hasn't read the upstream yet." }
        if preview.operation != nil {
            return "Finish the merge or rebase in progress first."
        }
        if let explanation = preview.relationExplanation { return explanation }
        guard let upstream = preview.upstream, upstream.oid != nil, !upstream.gone else {
            return "\(preview.upstream?.name ?? "The upstream") isn't there as last fetched. Fetch, then try again."
        }
        guard availableModes.contains(mode) else { return "Your Mac doesn't offer this way of syncing." }
        return modeBlockedReason(mode)
    }

    public var canUpdate: Bool { updateBlockReason == nil }

    /// Brings the incoming commits in the chosen way, against exactly the
    /// state and upstream commit this sheet showed. Fast-forward never
    /// falls back to a merge or rebase; the Mac refuses instead.
    public func update() async {
        guard canUpdate, let preview, let upstream = preview.upstream, let oid = upstream.oid else { return }
        let request = GitUpdateRequest(
            requestId: makeRequestID(), path: root, expected: preview.preconditions,
            upstream: upstream.name, upstreamOid: oid
        )
        sentRequestIDs.insert(request.requestId)
        let action: GitMutationAction = switch mode {
        case .fastForward: .fastForward(request)
        case .merge: .merge(request)
        case .rebase: .rebase(request)
        }
        await runner.perform(action) { [weak self] in await self?.load() }
    }

    // MARK: Fetch

    /// Remotes Fetch may name: the upstream's remote, or every remote when
    /// the branch tracks none (the person picks when there are several).
    public var fetchRemotes: [String] {
        guard let preview else { return [] }
        if let remote = preview.upstream?.remote { return [remote] }
        return preview.remotes.map(\.name)
    }

    public var fetchBlockReason: String? {
        guard support.fetch else { return "Your Mac doesn't offer fetching." }
        if let reason = blockReason() { return reason }
        guard preview != nil else { return "Latch hasn't read the upstream yet." }
        if fetchRemotes.isEmpty { return "This repository has no remotes." }
        return nil
    }

    public var canFetch: Bool { fetchBlockReason == nil }

    /// After an uncertain fetch whose receipt was read: the same remote,
    /// fetched as a new request. Never offered for anything but a fetch.
    public func fetchAgain() async {
        guard case .uncertain(.fetch(let previous), _) = runner.phase else { return }
        runner.acknowledge()
        await load()
        await fetch(remote: previous.remote)
    }

    public func fetch(remote: String) async {
        guard canFetch, let preview, fetchRemotes.contains(remote) else { return }
        let request = GitFetchRequest(
            requestId: makeRequestID(), path: root, expected: preview.preconditions, remote: remote
        )
        sentRequestIDs.insert(request.requestId)
        await runner.perform(.fetch(request)) { [weak self] in await self?.load() }
    }
}

// MARK: - Repository: slice 3

public extension GitRepositoryModel {
    // MARK: Sync and fetch

    var offersSync: Bool { offersChanges && support.sync && readSync != nil && support.updates }

    /// The Sync sheet's model, sharing this repository's runner.
    func syncModel() -> GitSyncModel? {
        guard offersSync, let readSync else { return nil }
        let hasAccess = hasAccess
        return GitSyncModel(
            root: target, runner: mutations, support: support,
            reading: { path, cursor in
                guard hasAccess() else { throw GitAccessError.unavailable }
                return try await readSync(path, cursor)
            },
            blockReason: { [weak self] in self?.mutationBlockReason },
            makeRequestID: makeRequestID,
            remember: { [weak self] preview in self?.syncPreview = preview }
        )
    }

    /// Why Sync is dimmed, or nil.
    var syncBlockReason: String? {
        if let reason = mutationBlockReason { return reason }
        if status?.operation != nil { return "Finish the merge or rebase in progress first." }
        return nil
    }

    var offersFetch: Bool { offersChanges && support.fetch }

    var fetchBlockReason: String? {
        guard offersFetch else { return nil }
        if let reason = mutationBlockReason { return reason }
        if status?.upstream == nil, readSync == nil {
            return "This branch doesn't track a remote. Fetch from the Sync sheet on your Mac."
        }
        return nil
    }

    var canFetch: Bool { offersFetch && fetchBlockReason == nil }

    /// The remote the header's Fetch uses, as the Mac named it; when the
    /// preview can't be read, the remote part of the upstream's name.
    /// Empty when there is none, several when the person must pick.
    func fetchRemotes() async -> [String] {
        if let readSync, hasAccess(), isOnline() {
            if let preview = try? await readSync(target, nil) {
                syncPreview = preview
                if let remote = preview.upstream?.remote { return [remote] }
                return preview.remotes.map(\.name)
            }
        }
        if let preview = syncPreview, let remote = preview.upstream?.remote { return [remote] }
        guard let upstream = status?.upstream?.name, let slash = upstream.firstIndex(of: "/") else { return [] }
        return [String(upstream[..<slash])]
    }

    /// Fetches `remote` against the status on screen. Fetching only updates
    /// remote-tracking refs; the status and preview are read again after.
    func fetch(remote: String) async {
        guard canFetch, let expected = status?.preconditions else { return }
        await mutations.perform(.fetch(GitFetchRequest(
            requestId: makeRequestID(), path: target, expected: expected, remote: remote
        )))
    }

    /// After an uncertain fetch whose receipt was read: a new fetch under a
    /// new request id. Never offered for a merge, rebase, continue or abort.
    func fetchAgain() async {
        guard case .uncertain(.fetch(let previous), _) = mutations.phase else { return }
        mutations.acknowledge()
        await refresh()
        await fetch(remote: previous.remote)
    }

    // MARK: Merge or rebase in progress

    var operationState: GitOperationState? { status.flatMap(GitOperationState.init(status:)) }

    var offersContinue: Bool {
        offersChanges && support.continueOperation && operationState?.controlled != nil
    }

    var offersAbort: Bool {
        offersChanges && support.abortOperation && operationState?.controlled != nil
    }

    var continueBlockReason: String? {
        if let reason = mutationBlockReason { return reason }
        return operationState?.continueBlockedReason
    }

    var canContinue: Bool { offersContinue && continueBlockReason == nil }

    var canAbort: Bool { offersAbort && mutationBlockReason == nil }

    /// Continues the merge or rebase. The Mac refuses with
    /// `unresolved_conflicts` while conflicted entries remain.
    func continueOperation() async {
        guard canContinue, let expected = status?.preconditions,
              let operation = operationState?.controlled else { return }
        await mutations.perform(.continueOperation(GitOperationControlRequest(
            requestId: makeRequestID(), path: target, expected: expected, operation: operation, confirmed: false
        )))
    }

    /// Aborts the merge or rebase. Only called after the person confirmed
    /// in the dialog, which is what `confirmed` tells the Mac.
    func abortOperation() async {
        guard canAbort, let expected = status?.preconditions,
              let operation = operationState?.controlled else { return }
        await mutations.perform(.abortOperation(GitOperationControlRequest(
            requestId: makeRequestID(), path: target, expected: expected, operation: operation, confirmed: true
        )))
    }

    // MARK: Conflict markers

    /// "<file> still contains conflict markers. Stage it anyway?" after the
    /// Mac refused to mark a conflicted file resolved, or nil.
    var conflictMarkersQuestion: String? {
        guard case .rejected(.stage(let request), let receipt) = mutations.phase,
              receipt.outcome?.code == "conflict_markers", !request.confirmConflictMarkers
        else { return nil }
        let named = receipt.outcome?.files ?? []
        let files = named.isEmpty
            ? request.files.map { file in
                status?.entries.first(where: { $0.pathKey == file.pathKey })?.path ?? file.pathKey
            }
            : named
        let subject: String
        switch files.count {
        case 0: subject = "This file still contains"
        case 1: subject = "\(files[0]) still contains"
        default: subject = "\(files.joined(separator: ", ")) still contain"
        }
        return "\(subject) conflict markers. Stage \(files.count > 1 ? "them" : "it") anyway?"
    }

    /// Stages the same files again with `confirmConflictMarkers`, as a new
    /// request: the refused one ran nothing.
    func confirmConflictMarkers() async {
        guard conflictMarkersQuestion != nil,
              case .rejected(.stage(let refused), _) = mutations.phase else { return }
        mutations.acknowledge()
        guard canMutate else { return }
        await mutations.perform(.stage(GitIndexRequest(
            requestId: makeRequestID(), path: refused.path, expected: status?.preconditions ?? refused.expected,
            files: refused.files.map { file in
                // The status was read again after the refusal; name the file
                // as it stands now if it is still listed as conflicted.
                let current = status?.entries.first { $0.pathKey == file.pathKey && $0.area == .conflicted }
                return GitFileRef(pathKey: file.pathKey, area: file.area, fingerprint: current?.fingerprint ?? file.fingerprint)
            },
            confirmConflictMarkers: true
        )))
    }

    func declineConflictMarkers() {
        guard conflictMarkersQuestion != nil else { return }
        mutations.acknowledge()
    }

    // MARK: Progress

    /// The progress view's content while a long change runs, unless the
    /// person chose "Stop waiting" for it.
    var progress: GitProgressPresentation? {
        guard let presentation = GitProgressPresentation.of(mutations.phase, lastReceipt: mutations.lastReceipt),
              presentation.requestId != progressDismissedFor
        else { return nil }
        return presentation
    }

    /// A long change still running whose progress view was dismissed.
    var isRunningInBackground: Bool {
        guard let presentation = GitProgressPresentation.of(mutations.phase, lastReceipt: mutations.lastReceipt)
        else { return false }
        return presentation.requestId == progressDismissedFor
    }

    /// Dismisses the progress view only. Git keeps running on the Mac and
    /// the receipt keeps being read (less often); nothing new can start
    /// until it settles.
    func stopWaiting() {
        guard let presentation = GitProgressPresentation.of(mutations.phase, lastReceipt: mutations.lastReceipt)
        else { return }
        progressDismissedFor = presentation.requestId
        mutations.isWatchingProgress = false
    }

    /// Back to the progress view: reads the receipt again now if no loop is
    /// already reading it.
    func resumeWaiting() async {
        progressDismissedFor = nil
        mutations.isWatchingProgress = true
        if case .pending = mutations.phase, !mutations.isPolling {
            await mutations.reconcile()
        } else if case .lost = mutations.phase {
            await mutations.reconcile()
        }
    }
}
