import Foundation
import Observation

/// Why a Git screen stopped working when the grant went away mid-view.
public enum GitAccessError: Error, Equatable, Sendable {
    case unavailable

    public var message: String {
        "Repositories require a current control grant."
    }
}

func gitMessage(for error: Error) -> String {
    if let error = error as? LatchError { return error.message }
    if let error = error as? GitAccessError { return error.message }
    return error.localizedDescription
}

// MARK: - Repositories sheet

/// The Repositories sheet: what the Mac reports, held only for as long as
/// the sheet is open. Nothing about these repositories is written to the
/// phone; the Mac owns the recent list.
@MainActor
@Observable
public final class GitRepositoryListModel {
    public typealias Listing = () async throws -> GitRepositoryList

    public private(set) var list: GitRepositoryList?
    public private(set) var isLoading = false
    public private(set) var error: String?

    private let listing: Listing
    private let hasAccess: () -> Bool
    private let isOnline: () -> Bool

    public init(
        listing: @escaping Listing,
        hasAccess: @escaping () -> Bool = { true },
        isOnline: @escaping () -> Bool = { true }
    ) {
        self.listing = listing
        self.hasAccess = hasAccess
        self.isOnline = isOnline
    }

    public func load() async {
        // Offline, the last answer stays; nothing is asked until the link is back.
        guard isOnline() else { return }
        guard hasAccess() else {
            error = GitAccessError.unavailable.message
            return
        }
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            list = try await listing()
            error = nil
        } catch {
            self.error = gitMessage(for: error)
        }
    }

    public var isEmpty: Bool {
        guard let list else { return false }
        return list.fromSessions.isEmpty && list.recent.isEmpty
    }
}

// MARK: - Repository screen

/// One repository on the Mac, as the repository screen shows it.
///
/// The last status stays readable when the link drops: the screen dims it
/// and says how old it is rather than blanking it, the same bargain the
/// session list makes. Changes go through ``mutations``, one at a time, and
/// only against a status read while the link is up.
@MainActor
@Observable
public final class GitRepositoryModel {
    public typealias Reading = (String) async throws -> GitStatus
    public typealias DiffReading = (String, String, GitChangeArea, String?) async throws -> GitFileDiff

    /// The folder the screen was opened for. After the first answer, every
    /// refresh asks for the root the Mac resolved, which it resolves again.
    public let folder: String
    public private(set) var status: GitStatus?
    public private(set) var isLoading = false
    public private(set) var error: String?
    /// When this phone received the status on screen.
    public private(set) var loadedAt: Date?

    /// The one change in flight for this repository, and its outcome.
    public let mutations: GitMutationRunner
    /// What this Mac lets the phone change.
    public let support: GitMutationSupport

    private let read: Reading
    private let readDiff: DiffReading
    private let readBranches: GitBranchesModel.Listing?
    let readSync: GitSyncModel.Reading?
    let readPush: GitPushModel.Reading?
    let hasAccess: () -> Bool
    let isOnline: () -> Bool
    private let now: () -> Date
    let makeRequestID: () -> String
    private let draftSupported: () -> Bool
    private let draftReadiness: () -> GitDraftReadinessState?
    private let generateDraft: GitCommitDraftModel.Generate?
    private let cancelDraft: GitCommitDraftModel.Cancel?
    /// Set when the link returns, until a status is read on it: every
    /// change waits for that fresh status, not just for the link.
    private(set) var awaitingFreshStatus = false
    /// The request whose progress view the person dismissed with "Stop
    /// waiting". Git keeps running and the receipt keeps being read.
    public internal(set) var progressDismissedFor: String?
    /// The latest Sync preview read for this repository, from the sheet or
    /// to learn which remote the header's Fetch should use.
    public internal(set) var syncPreview: GitSyncPreview?

    public init(
        folder: String,
        read: @escaping Reading,
        readDiff: @escaping DiffReading,
        hasAccess: @escaping () -> Bool = { true },
        isOnline: @escaping () -> Bool = { true },
        now: @escaping () -> Date = { Date() },
        support: GitMutationSupport = .none,
        mutate: GitMutationRunner.Sending? = nil,
        lookupReceipt: GitMutationRunner.Lookup? = nil,
        readBranches: GitBranchesModel.Listing? = nil,
        readSync: GitSyncModel.Reading? = nil,
        readPush: GitPushModel.Reading? = nil,
        checkRemote: GitMutationRunner.RemoteCheck? = nil,
        makeRequestID: @escaping () -> String = { UUID().uuidString.lowercased() },
        sleep: ((Double) async -> Void)? = nil,
        draftSupported: @escaping () -> Bool = { false },
        draftReadiness: @escaping () -> GitDraftReadinessState? = { nil },
        generateDraft: GitCommitDraftModel.Generate? = nil,
        cancelDraft: GitCommitDraftModel.Cancel? = nil
    ) {
        self.folder = folder
        self.read = read
        self.readDiff = readDiff
        self.hasAccess = hasAccess
        self.isOnline = isOnline
        self.now = now
        self.readBranches = readBranches
        self.readSync = readSync
        self.readPush = readPush
        self.makeRequestID = makeRequestID
        self.draftSupported = draftSupported
        self.draftReadiness = draftReadiness
        self.generateDraft = generateDraft
        self.cancelDraft = cancelDraft
        let wired = mutate != nil && lookupReceipt != nil
        self.support = wired ? support : .none
        let unavailable: GitMutationRunner.Sending = { _ in throw GitAccessError.unavailable }
        let noLookup: GitMutationRunner.Lookup = { _ in throw GitAccessError.unavailable }
        if let sleep {
            mutations = GitMutationRunner(
                send: mutate ?? unavailable, lookup: lookupReceipt ?? noLookup, isOnline: isOnline, sleep: sleep,
                now: now
            )
        } else {
            mutations = GitMutationRunner(
                send: mutate ?? unavailable, lookup: lookupReceipt ?? noLookup, isOnline: isOnline, now: now
            )
        }
        // "Check the remote" only on a Mac that advertises it.
        if wired, support.push, support.pushReconcile, let checkRemote {
            mutations.checkRemoteRoute = checkRemote
        }
        // Whatever the outcome, the screen shows the Mac's state next, not
        // a guess at what the change did.
        mutations.afterSettled = { [weak self] _ in await self?.refresh() }
    }

    /// Whether what is on screen is from before the current interruption.
    public var isStale: Bool {
        status != nil && (!isOnline() || awaitingFreshStatus || error != nil)
    }

    /// The outlined pill's words while stale.
    public func stalePill(now: Date = Date()) -> String {
        GitPresentation.stalePill(loadedAt: loadedAt, now: now)
    }

    /// The root the Mac resolved, or the folder until it has answered.
    public var target: String { status?.root ?? folder }

    public var sections: [GitSectionGroup] {
        GitSectionGroup.grouped(status?.entries ?? [])
    }

    /// Files in the order the screen lists them, for previous/next.
    public var orderedEntries: [GitStatusEntry] {
        sections.flatMap(\.entries)
    }

    public func refresh() async {
        // Offline, the last status stays readable and dimmed; nothing is
        // asked until the link is back.
        guard isOnline() else { return }
        guard hasAccess() else {
            error = GitAccessError.unavailable.message
            return
        }
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            status = try await read(target)
            loadedAt = now()
            awaitingFreshStatus = false
            error = nil
        } catch {
            // A failed refresh keeps what is on screen; the error says why
            // it did not update.
            self.error = gitMessage(for: error)
        }
    }

    // MARK: Changes

    /// Whether this Mac and this status allow any change at all. A Mac that
    /// predates mutations sends no preconditions, and nothing is offered.
    public var offersChanges: Bool {
        support.any && status?.preconditions != nil
    }

    /// Why a change cannot start right now, or nil when it can. Controls
    /// stay visible and dim; tapping one shows this.
    public var mutationBlockReason: String? {
        guard offersChanges else { return nil }
        if !isOnline() || isStale || awaitingFreshStatus {
            return "Reconnecting to your Mac. Changes wait for the link and a fresh status."
        }
        if !hasAccess() {
            return """
            This phone can't change this repository. Open Latch on your Mac, find this phone under             Remote Access, and set it to Control.
            """
        }
        switch mutations.phase {
        case .lost, .reconciling:
            return "Latch is checking what happened to your last change before starting another."
        case .uncertain(let action, _) where action.kind == .push && mutations.canCheckRemote:
            return "Check the remote to see what became of your last push before starting another change."
        case .uncertain:
            return "Refresh to see what your last change did before starting another."
        case .checkingRemote:
            return "Your Mac is checking the remote for your last push."
        case .sending, .pending:
            if let action = mutations.phase.action, action.isLongRunning {
                return "Git is still running your \(GitOutcomePresentation.noun(action.kind)) on your Mac. Latch keeps checking."
            }
            return "Waiting for your last change to finish."
        default:
            break
        }
        return isLoading ? "Reading the repository…" : nil
    }

    public var canMutate: Bool { offersChanges && mutationBlockReason == nil }

    /// Whether `entry` has a staging control at all. A conflicted file is
    /// resolved on the Mac; staging it here only marks it resolved, and
    /// only on a Mac that advertises that.
    public func offersStaging(_ entry: GitStatusEntry) -> Bool {
        guard offersChanges, entry.fingerprint != nil else { return false }
        switch entry.area {
        case .staged: return support.unstage
        case .unstaged, .untracked: return support.stage
        case .conflicted: return support.stageResolved
        }
    }

    public func canToggleStaging(_ entry: GitStatusEntry) -> Bool {
        offersStaging(entry) && canMutate
    }

    /// Stages or unstages one whole file, whichever its area calls for.
    public func toggleStaging(_ entry: GitStatusEntry) async {
        guard canToggleStaging(entry), let expected = status?.preconditions,
              let fingerprint = entry.fingerprint else { return }
        let request = GitIndexRequest(
            requestId: makeRequestID(), path: target, expected: expected,
            files: [GitFileRef(pathKey: entry.pathKey, area: entry.area, fingerprint: fingerprint)]
        )
        await mutations.perform(entry.area == .staged ? .unstage(request) : .stage(request))
    }

    /// The section header's "Stage all" or "Unstage all", if it has one.
    public func offersSectionAction(_ section: GitSection) -> Bool {
        guard offersChanges else { return false }
        switch section {
        case .staged: return support.unstage
        case .changes, .untracked: return support.stage
        case .conflicted: return false
        }
    }

    /// Everything in one section, as listed when the status was read.
    public func applyToSection(_ section: GitSection) async {
        guard offersSectionAction(section), canMutate, let expected = status?.preconditions else { return }
        let id = makeRequestID()
        let action: GitMutationAction = switch section {
        case .staged: .unstage(GitIndexRequest(requestId: id, path: target, expected: expected, all: [.staged]))
        case .changes: .stage(GitIndexRequest(requestId: id, path: target, expected: expected, all: [.unstaged]))
        case .untracked, .conflicted:
            .stage(GitIndexRequest(requestId: id, path: target, expected: expected, all: [.untracked]))
        }
        await mutations.perform(action)
    }

    public var stagedEntries: [GitStatusEntry] { status?.entries(in: .staged) ?? [] }

    /// Files with changes that a commit now would leave out.
    public var excludedFileCount: Int {
        let left = (status?.entries ?? []).filter { $0.area != .staged }.map(\.pathKey)
        return Set(left).count
    }

    /// Sessions in this worktree that are doing something right now.
    public var activeSessions: [GitSessionRef] { status?.sessions.filter(\.active) ?? [] }

    public var offersCommit: Bool { offersChanges && support.commit }

    /// Why Commit is off even though the phone could change things, or nil.
    public var commitBlockReason: String? {
        if let reason = mutationBlockReason { return reason }
        if status?.entriesTruncated == true {
            return "Latch cannot show every changed file. Review and commit on your Mac."
        }
        if status?.operation != nil {
            return "Finish the merge or rebase in progress on your Mac before committing."
        }
        if stagedEntries.isEmpty { return "Stage the files to include first." }
        return nil
    }

    public var canCommit: Bool { offersCommit && commitBlockReason == nil }

    public func commitDraftModel() -> GitCommitDraftModel {
        GitCommitDraftModel(
            repository: self,
            supported: { [weak self] in self?.draftSupported() == true && self?.generateDraft != nil },
            readiness: draftReadiness,
            generate: generateDraft ?? { _ in throw GitAccessError.unavailable },
            cancel: cancelDraft ?? { _ in }
        )
    }

    /// Commits exactly the staged files the person reviewed, on the branch
    /// they were shown. The summary is one line; the rest is the body.
    public func commit(summary: String, description: String) async {
        let summary = GitCommitMessage.summary(summary)
        guard canCommit, !summary.isEmpty, let expected = status?.preconditions else { return }
        let body = description.trimmingCharacters(in: .whitespacesAndNewlines)
        await mutations.perform(.commit(GitCommitRequest(
            requestId: makeRequestID(), path: target, expected: expected,
            summary: summary, description: body.isEmpty ? nil : body
        )))
    }

    public var offersBranches: Bool { support.branches && status != nil && readBranches != nil }

    /// The Branches sheet's model, sharing this repository's runner so only
    /// one change is ever in flight here.
    public func branchesModel() -> GitBranchesModel? {
        guard offersBranches, let readBranches else { return nil }
        let hasAccess = hasAccess
        let model = GitBranchesModel(
            root: target, runner: mutations, support: support,
            listing: { path in
                guard hasAccess() else { throw GitAccessError.unavailable }
                return try await readBranches(path)
            },
            makeRequestID: makeRequestID
        )
        return model
    }

    /// After the link returns: settle a lost or still-running change first,
    /// then read the repository again. Nothing new starts until that fresh
    /// status has arrived.
    public func reconnected() async {
        awaitingFreshStatus = status != nil
        await mutations.reconcile()
        await refresh()
    }

    /// Clears a seen outcome; an uncertain one also rereads the repository.
    public func acknowledgeOutcome() async {
        let wasUncertain: Bool = if case .uncertain = mutations.phase { true } else { false }
        mutations.acknowledge()
        if wasUncertain { await refresh() }
    }

    public func diffModel(for entry: GitStatusEntry) -> GitDiffModel {
        GitDiffModel(root: target, entry: entry, read: readDiff, hasAccess: hasAccess, isOnline: isOnline)
    }

    /// The file before or after `entry` in screen order.
    public func neighbor(of entry: GitStatusEntry, offset: Int) -> GitStatusEntry? {
        let entries = orderedEntries
        guard let index = entries.firstIndex(where: { $0.id == entry.id }) else { return nil }
        let target = index + offset
        return entries.indices.contains(target) ? entries[target] : nil
    }
}

// MARK: - Diff screen

/// One file's diff, read a page at a time. Pages already on screen stay when
/// the link drops.
@MainActor
@Observable
public final class GitDiffModel {
    public let root: String
    public let entry: GitStatusEntry
    public private(set) var diff: GitFileDiff?
    public private(set) var hunks: [GitDiffHunk] = []
    public private(set) var isLoading = false
    public private(set) var error: String?
    /// Set when the file changed between pages and the diff started over.
    public private(set) var notice: String?

    private let read: GitRepositoryModel.DiffReading
    private let hasAccess: () -> Bool
    private let isOnline: () -> Bool

    public init(
        root: String,
        entry: GitStatusEntry,
        read: @escaping GitRepositoryModel.DiffReading,
        hasAccess: @escaping () -> Bool = { true },
        isOnline: @escaping () -> Bool = { true }
    ) {
        self.root = root
        self.entry = entry
        self.read = read
        self.hasAccess = hasAccess
        self.isOnline = isOnline
    }

    public var isStale: Bool { diff != nil && !isOnline() }

    public var hasMore: Bool { diff?.nextCursor != nil }

    /// "Showing 2 of 3 hunks", while more remain.
    public var progressText: String? {
        guard let diff, diff.nextCursor != nil else { return nil }
        return "Showing \(hunks.count) of \(diff.totalHunks) \(diff.totalHunks == 1 ? "hunk" : "hunks")"
    }

    public func load() async {
        await fetch(cursor: nil, replacing: true)
    }

    public func loadMore() async {
        guard let cursor = diff?.nextCursor else { return }
        await fetch(cursor: cursor, replacing: false)
    }

    private func fetch(cursor: String?, replacing: Bool) async {
        guard isOnline() else { return }
        guard hasAccess() else {
            error = GitAccessError.unavailable.message
            return
        }
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let page = try await read(root, entry.pathKey, entry.area, cursor)
            hunks = replacing ? page.hunks : hunks + page.hunks.filter { hunk in
                !hunks.contains { $0.index == hunk.index }
            }
            diff = page
            error = nil
        } catch LatchError.git(code: "stale_cursor", _) {
            // The file changed under the reader. Start over rather than mix
            // hunks from two versions of it.
            isLoading = false
            notice = "The file changed on your Mac, so the diff was reloaded."
            await fetch(cursor: nil, replacing: true)
        } catch {
            self.error = gitMessage(for: error)
        }
    }
}
