import Foundation
import Observation

// MARK: - Review push

/// The push review, pushed inside the Sync sheet: the source branch, the
/// remote and destination as last fetched, and every commit to send, a page
/// at a time. Reading it never contacts the remote. The push is built only
/// from the preview on screen, and runs through the repository's one runner.
///
/// A branch without an upstream has no destination until the person
/// confirms one: the Mac's suggestion only pre-fills the fields, and the
/// review is read again for exactly what was confirmed.
@MainActor
@Observable
public final class GitPushModel {
    /// Folder, the confirmed destination (nil for the upstream), cursor.
    public typealias Reading = (String, GitPushSuggestion?, String?) async throws -> GitPushPreview

    public let root: String
    public private(set) var preview: GitPushPreview?
    /// Every outgoing commit read so far, newest first.
    public private(set) var outgoing: [GitCommitSummary] = []
    public private(set) var isLoading = false
    public private(set) var error: String?
    /// Set when the review was read again under the person (the refs moved
    /// between pages, or Push again found something different).
    public private(set) var notice: String?
    /// The remote and destination the person confirmed for a branch that
    /// has no upstream. Nil means "the upstream".
    public private(set) var selection: GitPushSuggestion?
    /// The destination fields while one must be chosen.
    public var draftRemote = ""
    public var draftDestination = ""
    /// Requests this screen sent, so it shows only their progress and outcomes.
    public private(set) var sentRequestIDs: Set<String> = []

    public let support: GitMutationSupport
    public let runner: GitMutationRunner
    private let reading: Reading
    private let blockReason: () -> String?
    private let makeRequestID: () -> String

    init(
        root: String, runner: GitMutationRunner, support: GitMutationSupport, reading: @escaping Reading,
        blockReason: @escaping () -> String?, makeRequestID: @escaping () -> String
    ) {
        self.root = root
        self.runner = runner
        self.support = support
        self.reading = reading
        self.blockReason = blockReason
        self.makeRequestID = makeRequestID
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
            let page = try await reading(root, selection, cursor)
            if cursor == nil {
                outgoing = page.outgoing
            } else {
                outgoing += page.outgoing.filter { commit in !outgoing.contains { $0.oid == commit.oid } }
            }
            preview = page
            error = nil
            prefillSelection(page)
        } catch LatchError.git(code: "stale_cursor", _) {
            // The branch or the destination moved between pages; mixing two
            // histories would misstate what is sent. Start over.
            isLoading = false
            await fetchPage(cursor: nil)
            notice = "The branch or the remote changed on your Mac, so the commits to push were reloaded."
        } catch {
            self.error = gitMessage(for: error)
        }
    }

    // MARK: Destination

    /// Whether the person must choose where to push before anything else.
    public var needsSelection: Bool {
        guard let preview else { return false }
        return preview.target == nil && preview.selectionRequired
    }

    /// Fills the fields from the Mac's suggestion, never over what the
    /// person typed. Filling them confirms nothing.
    private func prefillSelection(_ page: GitPushPreview) {
        guard page.target == nil, page.selectionRequired else { return }
        if draftRemote.isEmpty {
            draftRemote = page.suggestion?.remote ?? (page.remotes.count == 1 ? page.remotes[0].name : "")
        }
        if draftDestination.isEmpty {
            draftDestination = page.suggestion?.destination ?? page.source?.branch ?? ""
        }
    }

    public var remoteChoices: [String] { preview?.remotes.map(\.name) ?? [] }

    /// Why the chosen destination can't be reviewed yet, or nil.
    public var selectionBlockReason: String? {
        if remoteChoices.isEmpty { return "This repository has no remotes. Add one on your Mac." }
        guard remoteChoices.contains(draftRemote) else { return "Choose a remote." }
        let name = draftDestination.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return "Name the branch on the remote." }
        guard GitBranchName.isPlausible(name) else { return "Git won't accept that branch name." }
        return nil
    }

    public var canReviewSelection: Bool { selectionBlockReason == nil && !isLoading }

    /// The person confirmed a remote and destination: the review is read
    /// again for exactly that, and only then can it be pushed.
    public func reviewSelection() async {
        guard canReviewSelection else { return }
        selection = GitPushSuggestion(
            remote: draftRemote, destination: draftDestination.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        outgoing = []
        notice = nil
        await load()
    }

    /// Back to choosing, after a destination was confirmed.
    public func changeDestination() async {
        guard let current = selection else { return }
        draftRemote = current.remote
        draftDestination = current.destination
        selection = nil
        outgoing = []
        await load()
    }

    /// Whether the destination on screen is one the person may change:
    /// only one they chose, never the upstream.
    public var offersChangeDestination: Bool { selection != nil }

    // MARK: Push

    /// Why Push is off, or nil when it may run.
    public var pushBlockReason: String? {
        if let reason = blockReason() { return reason }
        if runner.unresolvedPush != nil {
            return "Check the remote for your last push before pushing again."
        }
        guard let preview else { return isLoading ? "Reading the push review…" : "Latch hasn't read the push review yet." }
        if let reason = preview.blockedReason { return reason }
        guard let target = preview.target else {
            return preview.selectionRequired
                ? "Choose where to push, then review."
                : "Your Mac didn't name a destination for this push."
        }
        // The destination must be the upstream, or exactly what the person
        // confirmed. Nothing is ever pushed somewhere chosen silently.
        switch target.selectedBy {
        case .upstream:
            guard selection == nil else { return "Review the destination you chose first." }
        case .request:
            guard let selection, selection.remote == target.remote, selection.destination == target.destination else {
                return "Review the destination you chose first."
            }
        case .unknown:
            return "Your Mac reported a destination this version of Latch doesn't know. Update Latch to push."
        }
        guard let source = preview.source else { return "Switch to a branch to push it." }
        if preview.operation != nil { return "Finish the merge or rebase in progress first." }
        if let ahead = preview.ahead, ahead == 0, !target.newBranch {
            return "Nothing to push. \(target.remote)/\(target.destination) already has \(source.branch) as last fetched."
        }
        return nil
    }

    public var canPush: Bool { support.push && pushBlockReason == nil }

    /// Pushes exactly what this review showed: its source commit, remote,
    /// destination and the destination's commit as last fetched, against
    /// the state the review was read from. A new request id every time.
    public func push() async {
        guard canPush, let preview, let target = preview.target, let source = preview.source else { return }
        let request = GitPushRequest(
            requestId: makeRequestID(), path: root, expected: preview.preconditions,
            remote: target.remote, remoteUrl: target.remoteUrl, destination: target.destination,
            sourceBranch: source.branch, sourceOid: source.oid, destinationOid: target.destinationOid
        )
        sentRequestIDs.insert(request.requestId)
        notice = nil
        await runner.perform(.push(request)) { [weak self] in await self?.load() }
    }

    /// After "Repository changed on your Mac": the review is read again and
    /// nothing is sent until the person taps Push on it.
    public func reviewAgain() async {
        switch runner.phase {
        case .rejected(.push, _), .refused(.push, _, _): runner.acknowledge()
        default: break
        }
        await load()
    }

    /// Only after the Mac's receipt said the push never ran (`not_applied`):
    /// the review is read again and, if it still sends the same commit to
    /// the same place, pushed as a new request. Anything different goes
    /// back to the person.
    public var offersPushAgain: Bool {
        guard case .rejected(.push, let receipt) = runner.phase else { return false }
        return receipt.outcome?.code == "not_applied"
    }

    public func pushAgain() async {
        guard offersPushAgain, case .rejected(.push(let previous), _) = runner.phase else { return }
        runner.acknowledge()
        await load()
        guard let preview, let target = preview.target, let source = preview.source,
              target.remote == previous.remote, target.destination == previous.destination,
              source.branch == previous.sourceBranch, source.oid == previous.sourceOid,
              target.destinationOid == previous.destinationOid
        else {
            notice = "The push review changed since the last push. Check it, then push."
            return
        }
        await push()
    }

    /// "Check the remote" for an uncertain push, or one put aside.
    public func checkRemote() async {
        await runner.checkRemote()
        if case .idle = runner.phase { return }
        await load()
    }
}

// MARK: - Repository: slice 4

public extension GitRepositoryModel {
    /// Review push sits inside the Sync sheet, so it needs that sheet, the
    /// preview and push routes, and receipts.
    var offersPush: Bool { offersSync && support.push && readPush != nil }

    /// "Check the remote" is offered only by a Mac that advertises it.
    var offersRemoteCheck: Bool { offersChanges && hasAccess() && mutations.offersRemoteCheck }

    /// Why Review push is dimmed, or nil.
    var pushBlockReason: String? {
        if let reason = mutationBlockReason { return reason }
        return nil
    }

    /// The push review's model, sharing this repository's runner.
    func pushModel() -> GitPushModel? {
        guard offersPush, let readPush else { return nil }
        let hasAccess = hasAccess
        return GitPushModel(
            root: target, runner: mutations, support: support,
            reading: { path, selection, cursor in
                guard hasAccess() else { throw GitAccessError.unavailable }
                return try await readPush(path, selection, cursor)
            },
            blockReason: { [weak self] in self?.mutationBlockReason },
            makeRequestID: makeRequestID
        )
    }

    /// Asks the Mac to check the remote for an uncertain push.
    func checkRemote() async {
        guard offersRemoteCheck else { return }
        await mutations.checkRemote()
    }

    /// The outcome card's content, with what Latch knows about any check of
    /// the remote.
    var outcomePresentation: GitOutcomePresentation? { GitOutcomePresentation.of(mutations) }

    /// Commits to push as last fetched, from the status.
    var outgoingCount: Int { status?.upstream?.ahead ?? 0 }
}

// MARK: - Presentation

public enum GitPushPresentation {
    public static let caption = "Uses the Git credentials on your Mac. Force push isn't available from the phone."

    /// "1 commit", "3 commits".
    public static func commits(_ count: Int) -> String {
        "\(count) \(count == 1 ? "commit" : "commits")"
    }

    /// A ref as VoiceOver should say it: "origin/main" is "origin main".
    public static func spoken(_ ref: String) -> String {
        ref.replacingOccurrences(of: "/", with: " ")
    }
}

public extension GitPushTarget {
    /// "origin/main".
    var name: String { "\(remote)/\(destination)" }
}

public extension GitPushPreview {
    /// "Push 2 commits to origin/main", or "Push main to origin/topic" when
    /// how many commits it sends isn't known.
    var pushButtonTitle: String {
        guard let target else { return "Push" }
        if let ahead, ahead > 0 {
            return "Push \(GitPushPresentation.commits(ahead)) to \(target.name)"
        }
        return "Push \(source?.branch ?? "HEAD") to \(target.name)"
    }

    /// "Push 2 commits from main to origin main".
    var pushSpokenLabel: String {
        guard let target else { return "Push" }
        let from = source?.branch ?? "HEAD"
        let to = GitPushPresentation.spoken(target.name)
        if let ahead, ahead > 0 {
            return "Push \(GitPushPresentation.commits(ahead)) from \(from) to \(to)"
        }
        return "Push \(from) to \(to)"
    }

    /// "main (this Mac · abc1234)".
    var sourceLine: String {
        guard let source else { return head.detached ? "Detached HEAD (this Mac)" : "\(head.branch ?? "HEAD") (this Mac · no commits)" }
        return "\(source.branch) (this Mac · \(GitPresentation.shortOid(source.oid)))"
    }

    /// "origin/main (remote · def5678)", "(new branch)", or "(not fetched)".
    var destinationLine: String? {
        guard let target else { return nil }
        if target.newBranch { return "\(target.name) (new branch)" }
        guard let oid = target.destinationOid else { return "\(target.name) (remote · not fetched)" }
        return "\(target.name) (remote · \(GitPresentation.shortOid(oid)))"
    }

    /// "main (this Mac · abc1234) → origin/main (remote · def5678)".
    var refsLine: String? {
        destinationLine.map { "\(sourceLine) \u{2192} \($0)" }
    }

    /// The header as VoiceOver says it, the arrow spoken as "to".
    var refsSpoken: String? {
        guard let target else { return nil }
        let from = source.map { "\($0.branch) on this Mac at \(GitPresentation.shortOid($0.oid))" } ?? sourceLine
        let to: String
        if target.newBranch {
            to = "new branch \(GitPresentation.spoken(target.name))"
        } else if let oid = target.destinationOid {
            to = "\(GitPresentation.spoken(target.name)) on the remote at \(GitPresentation.shortOid(oid))"
        } else {
            to = "\(GitPresentation.spoken(target.name)) on the remote"
        }
        return "\(from), to \(to)"
    }

    /// The neutral note when the destination is the remote's default branch.
    var defaultBranchNote: String? {
        guard let target, target.isDefaultBranch else { return nil }
        return "\(target.destination) is the default branch of \(target.remote). It gets the same review as any other branch."
    }

    /// The note when the push creates the destination.
    var newBranchNote: String? {
        guard let target, target.newBranch else { return nil }
        return "\(target.name) doesn't exist as last fetched. Pushing creates it on \(target.remote)."
    }

    /// "Outgoing ↑2".
    var outgoingHeader: String {
        guard let ahead, ahead > 0 else { return "Outgoing" }
        return "Outgoing \u{2191}\(ahead)"
    }

    /// The URL the remote card shows: the destination's, else the remote's.
    var remoteURL: String? {
        if let url = target?.remoteUrl { return url }
        guard let name = target?.remote else { return nil }
        return remotes.first { $0.name == name }?.url
    }

    func fetchedLine(now: Date = Date()) -> String {
        GitPresentation.fetchedPhrase(lastFetchedAt, now: now)
    }
}

extension GitPresentation {
    static func spoken(_ ref: String) -> String { GitPushPresentation.spoken(ref) }
}
