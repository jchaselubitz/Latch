import Foundation

/// Words and letters the repository screens show, kept out of the views so
/// every one of them is checked without a simulator. Nothing here decides
/// what the phone may do; it only says what the Mac reported.

// MARK: - Sections

/// The four lists of the repository screen, in the order they are shown.
public enum GitSection: String, CaseIterable, Sendable, Identifiable {
    case conflicted
    case staged
    case changes
    case untracked

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .conflicted: "Conflicted"
        case .staged: "Staged"
        case .changes: "Changes"
        case .untracked: "Untracked"
        }
    }

    public var area: GitChangeArea {
        switch self {
        case .conflicted: .conflicted
        case .staged: .staged
        case .changes: .unstaged
        case .untracked: .untracked
        }
    }

    public init(area: GitChangeArea) {
        switch area {
        case .conflicted: self = .conflicted
        case .staged: self = .staged
        case .unstaged: self = .changes
        case .untracked: self = .untracked
        }
    }
}

public struct GitSectionGroup: Equatable, Sendable, Identifiable {
    public let section: GitSection
    public let entries: [GitStatusEntry]
    public var id: GitSection { section }

    /// Non-empty sections in screen order, each keeping the Mac's order.
    public static func grouped(_ entries: [GitStatusEntry]) -> [GitSectionGroup] {
        GitSection.allCases.compactMap { section in
            let members = entries.filter { $0.area == section.area }
            return members.isEmpty ? nil : GitSectionGroup(section: section, entries: members)
        }
    }
}

// MARK: - Rows

public extension GitStatusEntry {
    /// The one-letter badge: M A D R ? and ! for a conflict. Copies and type
    /// changes borrow the closest letter Git itself uses.
    var badge: String {
        switch change {
        case .modified: "M"
        case .added: "A"
        case .deleted: "D"
        case .renamed: "R"
        case .copied: "C"
        case .typeChanged: "T"
        case .untracked: "?"
        case .conflicted: "!"
        }
    }

    /// The last path component, which is what the row leads with.
    var fileName: String {
        let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
        let name = (trimmed as NSString).lastPathComponent
        return path.hasSuffix("/") ? name + "/" : name
    }

    /// The row's second line: the folder, or "was <old name>" for a rename.
    var detail: String {
        if let origPath, change == .renamed || change == .copied {
            return "was \(origPath)"
        }
        let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
        let folder = (trimmed as NSString).deletingLastPathComponent
        return folder.isEmpty ? "" : folder
    }

    /// `+n −m`, or nil when the Mac had no counts (untracked, binary).
    var lineCounts: String? {
        guard let additions, let deletions, !binary else { return nil }
        return "+\(additions) \u{2212}\(deletions)"
    }

    /// What VoiceOver says the row is: "Modified, unstaged".
    var accessibilityStatus: String {
        let change: String = switch self.change {
        case .modified: "Modified"
        case .added: "Added"
        case .deleted: "Deleted"
        case .renamed: "Renamed"
        case .copied: "Copied"
        case .typeChanged: "Type changed"
        case .untracked: "Untracked"
        case .conflicted: "Conflicted"
        }
        let area: String? = switch self.area {
        case .staged: "staged"
        case .unstaged: "unstaged"
        case .conflicted, .untracked: nil
        }
        var parts = [change]
        if let area { parts.append(area) }
        if submodule { parts.append("submodule") }
        if binary { parts.append("binary") }
        if let additions, let deletions, !binary {
            parts.append("\(additions) added, \(deletions) removed")
        }
        return parts.joined(separator: ", ")
    }

    /// The staging circle's label, as the design spells it.
    var stagingLabel: String {
        switch area {
        case .staged: "Unstage \(fileName)"
        case .conflicted: "Stage \(fileName) as resolved"
        case .unstaged, .untracked: "Stage \(fileName)"
        }
    }
}

// MARK: - Header

public extension GitStatus {
    /// The branch, or the short commit a detached HEAD points at.
    var branchLabel: String {
        if let branch = head.branch { return branch }
        if let oid = head.oid { return String(oid.prefix(7)) }
        return "HEAD"
    }

    /// The chrome's status line: `main · ↑2 ↓1`.
    var chromeStatus: String {
        var parts = [branchLabel]
        if let counts = GitPresentation.aheadBehindArrows(upstream) { parts.append(counts) }
        return parts.joined(separator: " · ")
    }

    var changeCount: Int { entries.count }

    func entries(in section: GitSection) -> [GitStatusEntry] {
        entries.filter { $0.area == section.area }
    }
}

public enum GitPresentation {
    /// `↑2 ↓1`, leaving out a zero side; nil when in step or untracked.
    public static func aheadBehindArrows(_ upstream: GitUpstream?) -> String? {
        guard let upstream, let ahead = upstream.ahead, let behind = upstream.behind else { return nil }
        var parts: [String] = []
        if ahead > 0 { parts.append("\u{2191}\(ahead)") }
        if behind > 0 { parts.append("\u{2193}\(behind)") }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    /// Spoken form: "2 commits to push, 1 incoming".
    public static func aheadBehindSpoken(_ upstream: GitUpstream?) -> String {
        guard let upstream else { return "No upstream branch" }
        guard let ahead = upstream.ahead, let behind = upstream.behind else {
            return "\(upstream.name) is gone on this Mac"
        }
        if ahead == 0 && behind == 0 { return "Up to date with \(upstream.name)" }
        var parts: [String] = []
        if ahead > 0 { parts.append("\(ahead) \(ahead == 1 ? "commit" : "commits") to push") }
        if behind > 0 { parts.append("\(behind) incoming") }
        return parts.joined(separator: ", ")
    }

    /// "Tracks origin/main · fetched 4 min ago", always saying when, because
    /// the counts are only as fresh as the last fetch.
    public static func trackingLine(_ status: GitStatus, now: Date = Date()) -> String {
        guard let upstream = status.upstream else {
            return status.head.detached ? "Detached HEAD" : "No upstream branch"
        }
        return "Tracks \(upstream.name) · \(fetchedPhrase(status.lastFetchedAt, now: now))"
    }

    public static func fetchedPhrase(_ lastFetchedAt: String?, now: Date = Date()) -> String {
        guard let lastFetchedAt, let date = parseDate(lastFetchedAt) else { return "never fetched" }
        return "fetched \(relative(date, now: now))"
    }

    /// "just now", "4 min ago", "3 h ago", "2 d ago".
    public static func relative(_ date: Date, now: Date = Date()) -> String {
        let seconds = max(0, Int(now.timeIntervalSince(date)))
        switch seconds {
        case ..<60: return "just now"
        case ..<3600: return "\(seconds / 60) min ago"
        case ..<86_400: return "\(seconds / 3600) h ago"
        default: return "\(seconds / 86_400) d ago"
        }
    }

    /// "Reconnecting · showing what Latch saw 4 min ago".
    public static func stalePill(loadedAt: Date?, now: Date = Date()) -> String {
        guard let loadedAt else { return "Reconnecting" }
        let seconds = max(0, Int(now.timeIntervalSince(loadedAt)))
        let age = seconds < 60 ? "moments ago" : relative(loadedAt, now: now)
        return "Reconnecting · showing what Latch saw \(age)"
    }

    /// "Rebase stopped · 3 of 5", for the operation card.
    public static func operationTitle(_ operation: GitOperation) -> String {
        let name: String = switch operation.kind {
        case .merge: "Merge"
        case .rebase: "Rebase"
        case .cherryPick: "Cherry-pick"
        case .revert: "Revert"
        }
        if let step = operation.step, let total = operation.total {
            return "\(name) stopped · \(step) of \(total)"
        }
        return "\(name) in progress"
    }

    /// "Unstaged · +12 −4", for the diff screen's chrome.
    public static func diffStatus(area: GitChangeArea, diff: GitFileDiff?) -> String {
        let area: String = switch area {
        case .conflicted: "Conflicted"
        case .staged: "Staged"
        case .unstaged: "Unstaged"
        case .untracked: "Untracked"
        }
        guard let diff, diff.kind == .text else { return area }
        return "\(area) · +\(diff.additions) \u{2212}\(diff.deletions)"
    }

    /// Why a file has no text diff, as a MessageView says it.
    public static func noTextExplanation(_ kind: GitDiffKind) -> (icon: String, title: String, detail: String)? {
        switch kind {
        case .text:
            return nil
        case .binary:
            return ("doc.questionmark", "Binary file", "This file isn't text, so there are no lines to show. Open it on your Mac to see what changed.")
        case .tooLarge:
            return ("doc.text.magnifyingglass", "Too large to show", "This change is larger than Latch shows on a phone. Review it on your Mac.")
        case .submodule:
            return ("shippingbox", "Submodule", "This is another repository inside this one. Open it as its own repository to see its changes.")
        case .directory:
            return ("folder", "Untracked folder", "Git lists this new folder as one entry. Its files appear individually once one of them is added.")
        }
    }

    /// A time of day for the uncertain timeline, in the phone's locale.
    public static func clock(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return formatter.string(from: date)
    }

    public static func clock(_ text: String) -> String? {
        parseDate(text).map { clock($0) }
    }

    /// "45 s", "1 min 12 s", "1 h 3 min".
    public static func duration(_ interval: TimeInterval) -> String {
        let seconds = max(0, Int(interval))
        if seconds < 60 { return "\(seconds) s" }
        if seconds < 3600 {
            let rest = seconds % 60
            return rest == 0 ? "\(seconds / 60) min" : "\(seconds / 60) min \(rest) s"
        }
        return "\(seconds / 3600) h \((seconds % 3600) / 60) min"
    }

    /// `1a2b3c4` from a full object id.
    public static func shortOid(_ oid: String) -> String {
        oid.count == 40 || oid.count == 64 ? String(oid.prefix(7)) : oid
    }

    /// `main` from `refs/heads/main`.
    public static func shortRef(_ ref: String) -> String {
        for prefix in ["refs/heads/", "refs/remotes/", "refs/"] where ref.hasPrefix(prefix) {
            return String(ref.dropFirst(prefix.count))
        }
        return ref
    }

    public static func parseDate(_ text: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        if let date = formatter.date(from: text) { return date }
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: text)
    }
}

// MARK: - Sessions

public extension GitSessionRef {
    /// "Claude · git-ui · editing", the chip's words.
    var chipLabel: String {
        let kind: String = switch connector {
        case "claude": "Claude"
        case "codex": "Codex"
        case .some(let other): other.capitalized
        case .none: "Terminal"
        }
        let what = title ?? name
        return "\(kind) · \(what) · \(activityWord)"
    }

    var activityWord: String {
        switch agentPhase {
        case "working": return "working"
        case "awaiting_input": return "waiting for you"
        default: return active ? "active" : "idle"
        }
    }

    var accessibilityLabel: String {
        "\(chipLabel). \(active ? "Active" : "Idle")."
    }
}

public extension Array where Element == GitSessionRef {
    var activeCount: Int { filter(\.active).count }

    /// "2 sessions working here".
    var workingHereTitle: String {
        "\(count) \(count == 1 ? "session" : "sessions") working here"
    }
}

// MARK: - Commit message

public enum GitCommitMessage {
    /// The guide the summary field shows a count against.
    public static let summaryGuide = 50

    /// One line: a pasted newline becomes a space, and the ends are trimmed.
    public static func summary(_ text: String) -> String {
        text.components(separatedBy: .newlines)
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
    }

    /// "Commit 2 files to main".
    public static func buttonTitle(fileCount: Int, branch: String?) -> String {
        let files = "\(fileCount) \(fileCount == 1 ? "file" : "files")"
        return "Commit \(files) to \(branch ?? "detached HEAD")"
    }

    /// "3 unstaged files won't be included", or nil when none are left out.
    public static func exclusionNote(_ count: Int) -> String? {
        guard count > 0 else { return nil }
        return count == 1
            ? "1 unstaged file won't be included."
            : "\(count) unstaged files won't be included."
    }

    /// The orange note naming active sessions in this worktree.
    public static func activeSessionsWarning(_ sessions: [GitSessionRef]) -> String? {
        let active = sessions.filter(\.active)
        guard !active.isEmpty else { return nil }
        let names = active.prefix(2).map { $0.title ?? $0.name }.joined(separator: " and ")
        let more = active.count > 2 ? " and \(active.count - 2) more" : ""
        let verb = active.count == 1 ? "is" : "are"
        return "\(names)\(more) \(verb) active in this worktree. If files change before this runs, Latch will ask you to review again."
    }
}

// MARK: - Outcomes

/// How one change's outcome is shown: the four presentations of the design
/// (completed, rejected, refresh required, uncertain) plus the phone's own
/// "the answer was lost" state while it reads the receipt, and the
/// completed-but-stopped merge or rebase that now waits on the person.
public struct GitOutcomePresentation: Equatable, Sendable {
    public enum Style: Equatable, Sendable {
        case completed
        case rejected
        case refreshRequired
        case uncertain
        /// Git ran and stopped a merge or rebase part-way (conflicts, or a
        /// rebase step that needs the person). Not a failure, not a toast.
        case stopped
    }

    public enum NextStep: Equatable, Sendable {
        case dismiss
        case refresh
        case checkReceipt
        case commitChanges
        /// Fast-forward was refused; open the Sync sheet on Merge.
        case chooseMergeOrRebase
        /// Only for a fetch whose receipt was read and is uncertain:
        /// fetching is safe to repeat, as a new request.
        case fetchAgain
        /// A push the Mac could not confirm: ask it to look at the remote.
        case checkRemote
        /// Only after the receipt says the push never ran (`not_applied`).
        case pushAgain
        /// The push review changed on the Mac: read it again; the person
        /// confirms again before anything is sent.
        case reviewPushAgain
        /// The remote has commits this branch lacks: back to Sync.
        case fetchAndChooseUpdate
    }

    public var style: Style
    public var headline: String
    public var detail: String
    public var gitOutput: String?
    public var files: [String]
    public var primaryTitle: String
    public var nextStep: NextStep
    /// While true the primary button is off: the phone is asking the Mac.
    public var isChecking: Bool
    /// What Latch knows, oldest first, for an uncertain outcome.
    public var timeline: [String]
    /// A second button, when the outcome has one.
    public var secondaryTitle: String? = nil
    public var secondaryStep: NextStep? = nil

    /// The outcome of the runner's change, with what it knows about any
    /// check of the remote.
    @MainActor
    public static func of(_ runner: GitMutationRunner) -> GitOutcomePresentation? {
        of(
            runner.phase, lastReceipt: runner.lastReceipt, connectionLostAt: runner.connectionLostAt,
            canCheckRemote: runner.canCheckRemote, remoteCheckNote: runner.remoteCheckNote
        )
    }

    public static func of(
        _ phase: GitMutationPhase, lastReceipt: GitMutationReceipt? = nil, connectionLostAt: Date? = nil,
        canCheckRemote: Bool = false, remoteCheckNote: String? = nil
    ) -> GitOutcomePresentation? {
        switch phase {
        case .idle, .sending, .pending:
            return nil
        case .completed(let action, let receipt):
            if receipt.outcome?.code == "stopped" {
                return stopped(action, receipt)
            }
            return GitOutcomePresentation(
                style: .completed, headline: receipt.outcome?.message ?? completedFallback(action, receipt),
                detail: "",
                gitOutput: nil, files: [], primaryTitle: "Done", nextStep: .dismiss,
                isChecking: false, timeline: []
            )
        case .rejected(let action, let receipt):
            return rejected(action, receipt)
        case .refused(let action, let code, let message):
            if code == "refresh_required" || code == "stale_cursor" || code == "file_not_changed" {
                return refreshRequired(message, kind: action.kind)
            }
            return GitOutcomePresentation(
                style: .rejected, headline: "Your Mac didn't \(verb(action.kind))", detail: message,
                gitOutput: nil, files: [], primaryTitle: "OK", nextStep: .dismiss,
                isChecking: false, timeline: []
            )
        case .uncertain(let action, let receipt) where action.kind == .push:
            return uncertainPush(
                action, receipt: receipt, lostAt: connectionLostAt, canCheckRemote: canCheckRemote,
                note: remoteCheckNote, checking: false
            )
        case .checkingRemote(let action, let receipt):
            return uncertainPush(
                action, receipt: receipt, lostAt: connectionLostAt, canCheckRemote: true,
                note: nil, checking: true
            )
        case .uncertain(let action, let receipt):
            let code = receipt.outcome?.code
            let fallback = code == "timed_out"
                ? "Git ran longer than your Mac allows, so Latch stopped it. Refresh to see the repository as it is now."
                : "Your Mac stopped before it could say how this \(noun(action.kind)) ended. Refresh to see the repository as it is now."
            var presentation = GitOutcomePresentation(
                style: .uncertain, headline: "Latch can't tell whether this finished",
                detail: receipt.outcome?.message ?? fallback,
                gitOutput: receipt.outcome?.gitOutput, files: receipt.outcome?.files ?? [],
                primaryTitle: "Refresh", nextStep: .refresh, isChecking: false,
                timeline: timeline(
                    action, receipt: receipt, lostAt: connectionLostAt,
                    last: code == "timed_out" ? "Git ran out of time on your Mac." : "Your Mac couldn't confirm the result."
                )
            )
            // The receipt has been read, so a new fetch is safe; a merge,
            // rebase, continue or abort is never offered again from here.
            if action.kind == .fetch {
                presentation.secondaryTitle = "Fetch again"
                presentation.secondaryStep = .fetchAgain
            }
            return presentation
        case .lost(let action):
            return GitOutcomePresentation(
                style: .uncertain, headline: "The answer didn't arrive",
                detail: "Latch sent this \(noun(action.kind)) to your Mac, but the connection dropped before it answered. Latch checks the receipt on your Mac before you can try again.",
                gitOutput: nil, files: [], primaryTitle: "Check again", nextStep: .checkReceipt,
                isChecking: false,
                timeline: timeline(action, receipt: lastReceipt, lostAt: connectionLostAt, last: "Connection lost.")
            )
        case .reconciling(let action):
            return GitOutcomePresentation(
                style: .uncertain, headline: "Checking the receipt on your Mac…",
                detail: "Latch is asking your Mac what happened to this \(noun(action.kind)).",
                gitOutput: nil, files: [], primaryTitle: "Check again", nextStep: .checkReceipt,
                isChecking: true,
                timeline: timeline(action, receipt: lastReceipt, lostAt: connectionLostAt, last: "Checking the receipt…")
            )
        }
    }

    /// A push Latch can't confirm: what it knows, oldest first, and the
    /// one way to learn more, asking the Mac to look at the remote. It
    /// resolves to "Pushed", "Git rejected the push" or "No sign the push
    /// ran"; only the last offers Push again.
    private static func uncertainPush(
        _ action: GitMutationAction, receipt: GitMutationReceipt, lostAt: Date?, canCheckRemote: Bool,
        note: String?, checking: Bool
    ) -> GitOutcomePresentation {
        let code = receipt.outcome?.code
        let fallback: String = switch code {
        case "timed_out": "Git ran longer than your Mac allows, so it was stopped. The push may or may not have reached the remote."
        case "interrupted": "Your Mac stopped before Git finished. The push may or may not have reached the remote."
        default: "Your Mac couldn't confirm whether the remote took the push."
        }
        var detail = receipt.outcome?.message ?? fallback
        if let note, !note.isEmpty { detail += "\n\n" + note }
        if !canCheckRemote {
            detail += " Fetch on your Mac to see where the remote is now."
        }
        var steps = timeline(
            action, receipt: receipt, lostAt: lostAt,
            last: code == "timed_out" ? "Git ran out of time on your Mac." : "Your Mac couldn't confirm the result."
        )
        if checking { steps.append("Checking the remote…") }
        var presentation = GitOutcomePresentation(
            style: .uncertain,
            headline: checking ? "Checking the remote…" : "Latch can't tell whether the push reached the remote",
            detail: detail,
            gitOutput: receipt.outcome?.gitOutput, files: [],
            primaryTitle: canCheckRemote ? "Check the remote" : "Refresh",
            nextStep: canCheckRemote ? .checkRemote : .refresh,
            isChecking: checking, timeline: steps
        )
        if canCheckRemote {
            presentation.secondaryTitle = "Later"
            presentation.secondaryStep = .refresh
        }
        return presentation
    }

    static func completedFallback(_ action: GitMutationAction, _ receipt: GitMutationReceipt) -> String {
        guard case .push(let request) = action else { return "Done." }
        let target = "\(request.remote)/\(request.destination)"
        return switch receipt.outcome?.code {
        case "up_to_date": "\(target) is already up to date."
        case "already_on_remote": "The remote already had these commits."
        default: "Pushed to \(target)."
        }
    }

    private static func stopped(_ action: GitMutationAction, _ receipt: GitMutationReceipt) -> GitOutcomePresentation {
        let outcome = receipt.outcome
        let files = outcome?.files ?? []
        let operation = switch action.kind {
        case .rebase: "rebase"
        case .merge: "merge"
        default: "merge or rebase"
        }
        let headline = files.isEmpty
            ? "Git stopped the \(operation)"
            : "Git stopped on conflicts in \(files.count) \(files.count == 1 ? "file" : "files")"
        let detail = files.isEmpty
            ? (outcome?.message ?? "Git needs you before the \(operation) can go on. Finish the step on your Mac, then continue here.")
            : "Resolve on your Mac, then stage them here."
        return GitOutcomePresentation(
            style: .stopped, headline: headline, detail: detail,
            gitOutput: outcome?.gitOutput, files: files, primaryTitle: "OK", nextStep: .dismiss,
            isChecking: false, timeline: []
        )
    }

    private static func rejected(_ action: GitMutationAction, _ receipt: GitMutationReceipt) -> GitOutcomePresentation {
        let outcome = receipt.outcome
        let code = outcome?.code ?? "git_refused"
        if code == "refresh_required" {
            return refreshRequired(outcome?.message, kind: action.kind)
        }
        if action.kind == .push {
            return rejectedPush(action, receipt)
        }
        let headline: String = switch code {
        case "would_overwrite":
            action.kind == .switchBranch ? "Git didn't switch branches" : "Local changes would be overwritten"
        case "nothing_to_commit": "Nothing to commit"
        case "identity_missing": "Git doesn't know who you are on this Mac"
        case "signing_failed": "Git couldn't sign the commit"
        case "hook_failed": "A Git hook on your Mac stopped this"
        case "branch_exists": "That branch already exists"
        case "invalid_branch_name": "Git won't accept that branch name"
        case "operation_in_progress": "Finish the merge or rebase first"
        case "not_found": "That branch is gone"
        case "not_applied": "Nothing changed"
        case "not_fast_forward": "This branch can't be fast-forwarded"
        case "unresolved_conflicts": "Conflicted files remain"
        case "conflict_markers": "Conflict markers remain"
        case "no_operation": "No merge or rebase is in progress"
        case "no_upstream": "This branch has no upstream"
        case "remote_not_found": "Git can't find that remote"
        case "auth_failed": "The remote didn't accept your Mac's credentials"
        case "network_failed": "Your Mac couldn't reach the remote"
        case "local_changes": "Local changes are in the way"
        default: "Git didn't \(verb(action.kind))"
        }
        let next: NextStep = switch code {
        case "would_overwrite", "local_changes": .commitChanges
        case "not_fast_forward": .chooseMergeOrRebase
        default: .refresh
        }
        let primary: String = switch next {
        case .commitChanges: "Commit changes…"
        case .chooseMergeOrRebase: "Choose Merge or Rebase"
        default: "Refresh"
        }
        var detail = outcome?.message ?? "Git on your Mac refused."
        if code == "unresolved_conflicts" {
            detail += " Resolve them on your Mac or in a terminal session, then stage them here."
        }
        return GitOutcomePresentation(
            style: .rejected, headline: headline,
            detail: detail,
            gitOutput: outcome?.gitOutput, files: outcome?.files ?? [],
            primaryTitle: primary, nextStep: next,
            isChecking: false, timeline: []
        )
    }

    private static func refreshRequired(_ message: String?, kind: GitMutationKind = .unknown) -> GitOutcomePresentation {
        if kind == .push {
            return GitOutcomePresentation(
                style: .refreshRequired, headline: "Repository changed on your Mac",
                detail: message ?? "The branch, its commits or the remote changed after the push was reviewed, so nothing was pushed. Review it again.",
                gitOutput: nil, files: [], primaryTitle: "Review again", nextStep: .reviewPushAgain,
                isChecking: false, timeline: []
            )
        }
        return GitOutcomePresentation(
            style: .refreshRequired, headline: "Repository changed on your Mac",
            detail: message ?? "Something changed after this screen was read, so Latch didn't act on it. Refresh and review again.",
            gitOutput: nil, files: [], primaryTitle: "Refresh", nextStep: .refresh,
            isChecking: false, timeline: []
        )
    }

    /// A push Git or the remote refused, with the next sensible step.
    private static func rejectedPush(_ action: GitMutationAction, _ receipt: GitMutationReceipt) -> GitOutcomePresentation {
        let outcome = receipt.outcome
        let code = outcome?.code ?? "git_refused"
        var target = "the remote"
        if case .push(let request) = action { target = "\(request.remote)/\(request.destination)" }
        let headline: String
        var next: NextStep = .refresh
        var primary = "OK"
        var extra: String?
        switch code {
        case "non_fast_forward":
            headline = "\(target) has commits this branch doesn't"
            next = .fetchAndChooseUpdate
            primary = "Fetch and choose how to update"
            extra = "Bring them in first, then push again. Force push isn't available from the phone."
        case "remote_rejected":
            headline = "The remote rejected the push"
            extra = "The remote may protect this branch or run a check that refused it. Git's output says why."
        case "auth_failed":
            headline = "The remote didn't accept your Mac's credentials"
            extra = "Set up Git credentials for this remote on your Mac, then push again."
        case "network_failed":
            headline = "Your Mac couldn't reach the remote"
        case "remote_not_found":
            headline = "Git can't find that remote"
        case "hook_failed":
            headline = "A pre-push hook on your Mac stopped the push"
        case "not_applied":
            headline = "No sign the push ran"
            next = .pushAgain
            primary = "Push again"
            extra = "Your Mac found no evidence the push reached \(target), so it's safe to push again."
        case "git_refused":
            headline = "Git rejected the push"
        default:
            headline = "Git rejected the push"
        }
        var detail = outcome?.message ?? "Git on your Mac refused."
        if let extra { detail += " " + extra }
        return GitOutcomePresentation(
            style: .rejected, headline: headline, detail: detail,
            gitOutput: outcome?.gitOutput, files: [], primaryTitle: primary, nextStep: next,
            isChecking: false, timeline: []
        )
    }

    /// What Latch knows, oldest first: the request, when the Mac accepted
    /// it and started Git, Git's last progress, and the connection loss.
    private static func timeline(
        _ action: GitMutationAction, receipt: GitMutationReceipt?, lostAt: Date?, last: String
    ) -> [String] {
        var steps = ["Sent \(noun(action.kind)) · Receipt \(action.requestId.prefix(8))"]
        if let receipt {
            steps.append("Your Mac accepted it\(GitPresentation.clock(receipt.acceptedAt).map { " at \($0)" } ?? "").")
            if let started = receipt.startedAt {
                steps.append("Git started\(GitPresentation.clock(started).map { " at \($0)" } ?? "").")
            }
            if let progress = receipt.progress {
                let when = GitPresentation.clock(progress.updatedAt).map { " (\($0))" } ?? ""
                steps.append("Last progress: \(progress.message)\(when)")
            }
        } else {
            steps.append("Waiting for your Mac to answer.")
        }
        if let lostAt, last != "Connection lost." {
            steps.append("Connection lost at \(GitPresentation.clock(lostAt)).")
        }
        steps.append(lostAt.map { last == "Connection lost." ? "Connection lost at \(GitPresentation.clock($0))." : last } ?? last)
        return steps
    }

    public static func noun(_ kind: GitMutationKind) -> String {
        switch kind {
        case .stage: "stage"
        case .unstage: "unstage"
        case .commit: "commit"
        case .createBranch: "new branch"
        case .switchBranch: "branch switch"
        case .fetch: "fetch"
        case .fastForward: "fast-forward"
        case .merge: "merge"
        case .rebase: "rebase"
        case .continueOperation: "continue"
        case .abortOperation: "abort"
        case .push: "push"
        case .unknown: "change"
        }
    }

    public static func verb(_ kind: GitMutationKind) -> String {
        switch kind {
        case .stage: "stage that"
        case .unstage: "unstage that"
        case .commit: "commit"
        case .createBranch: "create the branch"
        case .switchBranch: "switch branches"
        case .fetch: "fetch"
        case .fastForward: "fast-forward"
        case .merge: "merge"
        case .rebase: "rebase"
        case .continueOperation: "continue"
        case .abortOperation: "abort"
        case .push: "push"
        case .unknown: "make that change"
        }
    }
}

// MARK: - Progress

/// What the progress view shows while a long change runs on the Mac. It
/// only reports: nothing here says Git stopped.
public struct GitProgressPresentation: Equatable, Sendable {
    public var title: String
    public var message: String
    /// 0...1, when Git reported a percentage.
    public var fraction: Double?
    /// When Git started, or when the Mac accepted the request.
    public var since: Date?
    public var requestId: String
    /// What "Stop waiting" means for this change: it never stops Git.
    public var stopWaitingNote = "Git runs on your Mac. If you stop waiting it keeps going, and Latch checks the result before you start anything else."

    public static func of(_ phase: GitMutationPhase, lastReceipt: GitMutationReceipt? = nil) -> GitProgressPresentation? {
        let action: GitMutationAction
        let receipt: GitMutationReceipt?
        switch phase {
        case .sending(let sending): action = sending; receipt = nil
        case .pending(let pending, let current): action = pending; receipt = current
        case .reconciling(let checking):
            // Only a change last seen running; a lost answer with no receipt
            // is the uncertain card's "Checking the receipt" instead.
            guard lastReceipt?.state == .pending else { return nil }
            action = checking; receipt = lastReceipt
        default: return nil
        }
        guard action.isLongRunning else { return nil }
        let message: String
        if case .reconciling = phase {
            message = "Checking the receipt on your Mac…"
        } else if let progress = receipt?.progress, !progress.message.isEmpty {
            message = progress.message
        } else if receipt == nil {
            message = "Sending to your Mac…"
        } else {
            message = "Waiting for Git on your Mac…"
        }
        let since = (receipt?.startedAt ?? receipt?.acceptedAt).flatMap(GitPresentation.parseDate)
        let fraction = receipt?.progress?.percent.map { Double(min(max($0, 0), 100)) / 100 }
        var presentation = GitProgressPresentation(
            title: title(action), message: message, fraction: fraction, since: since,
            requestId: action.requestId
        )
        if action.kind == .push {
            presentation.stopWaitingNote = "Git may still be pushing from your Mac. Stopping waiting doesn't stop the push; Latch checks the result before you push again."
        }
        return presentation
    }

    static func title(_ action: GitMutationAction) -> String {
        switch action {
        case .fetch(let request): "Fetching \(request.remote)…"
        case .fastForward(let request): "Fast-forwarding to \(request.upstream)…"
        case .merge(let request): "Merging \(request.upstream)…"
        case .rebase(let request): "Rebasing onto \(request.upstream)…"
        case .continueOperation(let request): request.operation == .rebase ? "Continuing the rebase…" : "Continuing the merge…"
        case .abortOperation(let request): request.operation == .rebase ? "Aborting the rebase…" : "Aborting the merge…"
        case .push(let request): "Pushing \(request.sourceBranch) to \(request.remote)/\(request.destination)…"
        default: "Working…"
        }
    }

    /// "Running for 1 min 12 s".
    public func elapsed(now: Date = Date()) -> String? {
        guard let since else { return nil }
        return "Running for \(GitPresentation.duration(now.timeIntervalSince(since)))"
    }
}

// MARK: - Merge or rebase in progress

/// The orange operation card and the Continue/Abort bar, from the status.
public struct GitOperationState: Equatable, Sendable {
    public var operation: GitOperation
    /// Merge or rebase; nil for a cherry-pick or revert, which the phone
    /// neither continues nor aborts.
    public var controlled: GitControlledOperation?
    public var conflictedCount: Int

    public init(operation: GitOperation, conflictedCount: Int) {
        self.operation = operation
        self.conflictedCount = conflictedCount
        controlled = switch operation.kind {
        case .merge: .merge
        case .rebase: .rebase
        case .cherryPick, .revert: nil
        }
    }

    public init?(status: GitStatus) {
        guard let operation = status.operation else { return nil }
        self.init(operation: operation, conflictedCount: status.entries(in: .conflicted).count)
    }

    var name: String {
        switch operation.kind {
        case .merge: "Merge"
        case .rebase: "Rebase"
        case .cherryPick: "Cherry-pick"
        case .revert: "Revert"
        }
    }

    /// "Rebase in progress".
    public var title: String { "\(name) in progress" }

    /// "Rebasing main onto 1a2b3c4", "Merging origin/main into main".
    public func refs(branch: String?) -> String? {
        switch operation.kind {
        case .rebase:
            let head = operation.headName.map(GitPresentation.shortRef) ?? branch
            guard let head else { return operation.onto.map { "Rebasing onto \(GitPresentation.shortOid($0))" } }
            if let onto = operation.onto { return "Rebasing \(head) onto \(GitPresentation.shortOid(onto))" }
            return "Rebasing \(head)"
        case .merge:
            let incoming = operation.summary ?? operation.mergeHead.map(GitPresentation.shortOid)
            switch (incoming, branch) {
            case let (incoming?, branch?): return "Merging \(incoming) into \(branch)"
            case let (incoming?, nil): return "Merging \(incoming)"
            default: return nil
            }
        case .cherryPick, .revert:
            return nil
        }
    }

    /// "Stopped at commit 3 of 5".
    public var stepLine: String? {
        guard let step = operation.step, let total = operation.total else { return nil }
        return "Stopped at commit \(step) of \(total)"
    }

    /// "2 conflicted files", or nil with none.
    public var conflictLine: String? {
        guard conflictedCount > 0 else { return nil }
        return "\(conflictedCount) conflicted \(conflictedCount == 1 ? "file" : "files")"
    }

    public static let guidance = "Resolve the conflicted files on your Mac or in a terminal session, then stage them here."

    public var canContinue: Bool { conflictedCount == 0 }

    /// Why Continue is dimmed, or nil when it isn't.
    public var continueBlockedReason: String? {
        guard conflictedCount > 0 else { return nil }
        return conflictedCount == 1
            ? "Stage the conflicted file once it's resolved, then continue."
            : "Stage the \(conflictedCount) conflicted files once they're resolved, then continue."
    }

    /// "Continue rebase", "Abort merge" — the VoiceOver labels.
    public var continueLabel: String { "Continue \(name.lowercased())" }
    public var abortLabel: String { "Abort \(name.lowercased())" }

    public var abortConfirmation: String {
        switch operation.kind {
        case .rebase: "Abort the rebase? Your branch goes back to where it was before the rebase started."
        default: "Abort the merge? Your branch goes back to where it was before the merge started."
        }
    }
}

// MARK: - Sync

/// The three ways the Sync sheet brings incoming commits in.
public enum GitSyncMode: String, CaseIterable, Sendable, Identifiable {
    case fastForward
    case merge
    case rebase

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .fastForward: "Fast-forward only"
        case .merge: "Merge"
        case .rebase: "Rebase"
        }
    }

    public var shortTitle: String {
        switch self {
        case .fastForward: "Fast-forward"
        case .merge: "Merge"
        case .rebase: "Rebase"
        }
    }

    public var explanation: String {
        switch self {
        case .fastForward: "Moves your branch up to the incoming commits. Only works when you have no commits of your own on top."
        case .merge: "Adds a merge commit that joins the incoming commits with yours."
        case .rebase: "Replays your commits on top of the incoming ones, rewriting them."
        }
    }

    /// "Fast-forward main", "Merge origin/main into main", "Rebase main onto origin/main".
    public func buttonTitle(upstream: String, branch: String) -> String {
        switch self {
        case .fastForward: "Fast-forward \(branch)"
        case .merge: "Merge \(upstream) into \(branch)"
        case .rebase: "Rebase \(branch) onto \(upstream)"
        }
    }
}

public extension GitSyncPreview {
    /// "origin/main → main".
    var refsLine: String? {
        guard let upstream else { return nil }
        return "\(upstream.name) \u{2192} \(head.branch ?? "HEAD")"
    }

    /// "↑2 to push", while outgoing commits exist. Push is not offered here.
    var outgoingLine: String? {
        guard let ahead, ahead > 0 else { return nil }
        return "\u{2191}\(ahead) to push"
    }

    var incomingCount: Int { behind ?? incoming.count }

    /// The remote card's second line: "fetched 4 min ago".
    func fetchedLine(now: Date = Date()) -> String {
        GitPresentation.fetchedPhrase(lastFetchedAt, now: now)
    }

    /// Why nothing can be brought in from this preview, independent of mode.
    var relationExplanation: String? {
        switch relation {
        case .upToDate: return "\(head.branch ?? "This branch") is up to date with \(upstream?.name ?? "its upstream") as last fetched."
        case .ahead: return "Nothing incoming. \(head.branch ?? "This branch") only has commits of its own."
        case .noUpstream: return "\(head.branch ?? "This branch") doesn't track a remote branch. Set one up on your Mac."
        case .upstreamGone: return "\(upstream?.name ?? "The upstream") no longer exists on the remote as last fetched."
        case .detached: return "HEAD is detached. Switch to a branch to sync it."
        case .unborn: return "This branch has no commits yet."
        case .unknown: return "Your Mac reported a state this version of Latch doesn't know. Update Latch to sync."
        case .behind, .diverged: return nil
        }
    }
}

public extension GitCommitSummary {
    var shortOid: String { String(oid.prefix(7)) }

    /// "Ana · 3 h ago".
    func byline(now: Date = Date()) -> String {
        var parts: [String] = []
        if !author.isEmpty { parts.append(author) }
        if let committedAt, let date = GitPresentation.parseDate(committedAt) {
            parts.append(GitPresentation.relative(date, now: now))
        }
        return parts.joined(separator: " · ")
    }
}

public extension GitBranch {
    /// "origin/main · ↑2 ↓1", "origin/x is gone", or nil without upstream.
    var upstreamLine: String? {
        guard let upstream else { return nil }
        if upstreamGone { return "\(upstream) is gone" }
        let counts = GitPresentation.aheadBehindArrows(GitUpstream(name: upstream, ahead: ahead, behind: behind))
        return counts.map { "\(upstream) · \($0)" } ?? upstream
    }
}
