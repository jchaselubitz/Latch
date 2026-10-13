import Foundation
import Observation

// MARK: - Actions

/// One change the phone asked the Mac to make, with the request exactly as
/// it was sent. A request whose answer was lost is only ever sent again in
/// this form, under the same request id, so the Mac's receipt can recognise
/// it and never run Git twice.
public enum GitMutationAction: Equatable, Sendable {
    case stage(GitIndexRequest)
    case unstage(GitIndexRequest)
    case commit(GitCommitRequest)
    case createBranch(GitCreateBranchRequest)
    case switchBranch(GitSwitchBranchRequest)
    case fetch(GitFetchRequest)
    case fastForward(GitUpdateRequest)
    case merge(GitUpdateRequest)
    case rebase(GitUpdateRequest)
    case continueOperation(GitOperationControlRequest)
    case abortOperation(GitOperationControlRequest)
    /// Slice 4: a reviewed push. Never retried under a new id unless the
    /// Mac showed it never ran.
    case push(GitPushRequest)

    public var requestId: String {
        switch self {
        case .stage(let request), .unstage(let request): request.requestId
        case .commit(let request): request.requestId
        case .createBranch(let request): request.requestId
        case .switchBranch(let request): request.requestId
        case .fetch(let request): request.requestId
        case .fastForward(let request), .merge(let request), .rebase(let request): request.requestId
        case .continueOperation(let request), .abortOperation(let request): request.requestId
        case .push(let request): request.requestId
        }
    }

    public var kind: GitMutationKind {
        switch self {
        case .stage: .stage
        case .unstage: .unstage
        case .commit: .commit
        case .createBranch: .createBranch
        case .switchBranch: .switchBranch
        case .fetch: .fetch
        case .fastForward: .fastForward
        case .merge: .merge
        case .rebase: .rebase
        case .continueOperation: .continueOperation
        case .abortOperation: .abortOperation
        case .push: .push
        }
    }

    /// Whether the outcome may have changed which branch is checked out.
    public var touchesBranches: Bool {
        switch self {
        case .createBranch, .switchBranch, .commit, .fetch, .fastForward, .merge, .rebase,
             .continueOperation, .abortOperation, .push: true
        case .stage, .unstage: false
        }
    }

    /// Fetching, updating a branch, and continuing or aborting a merge or
    /// rebase can run for minutes: the Mac answers with a pending receipt
    /// carrying Git's progress, and the phone keeps reading it.
    public var isLongRunning: Bool { kind.isLongRunning }

    /// Conflicted files marked resolved: the Mac only takes these when it
    /// advertises `gitStageResolved`.
    public var stagesConflictedFiles: Bool {
        guard case .stage(let request) = self else { return false }
        return request.confirmConflictMarkers || request.files.contains { $0.area == .conflicted }
    }
}

public extension GitMutationKind {
    var isLongRunning: Bool {
        switch self {
        case .fetch, .fastForward, .merge, .rebase, .continueOperation, .abortOperation, .push: true
        case .stage, .unstage, .commit, .createBranch, .switchBranch, .unknown: false
        }
    }
}

/// Which mutations this Mac serves, each advertised on its own. Every one
/// also needs the receipt route: without it a lost answer could not be
/// checked, so the action is not offered at all.
public struct GitMutationSupport: Equatable, Sendable {
    public var stage: Bool
    public var unstage: Bool
    public var commit: Bool
    public var branches: Bool
    public var createBranch: Bool
    public var switchBranch: Bool
    /// Slice 3: the Sync sheet's preview, fetch, the three ways to update a
    /// branch, Continue/Abort of a merge or rebase, and staging a conflicted
    /// file as resolved. Each is advertised on its own.
    public var sync: Bool
    public var fetch: Bool
    public var fastForward: Bool
    public var merge: Bool
    public var rebase: Bool
    public var continueOperation: Bool
    public var abortOperation: Bool
    public var stageResolved: Bool
    /// Slice 4: the push review and push, offered only together (and only
    /// with receipts); "Check the remote" is advertised on its own.
    public var push: Bool
    public var pushReconcile: Bool

    public init(
        stage: Bool = false, unstage: Bool = false, commit: Bool = false, branches: Bool = false,
        createBranch: Bool = false, switchBranch: Bool = false,
        sync: Bool = false, fetch: Bool = false, fastForward: Bool = false, merge: Bool = false,
        rebase: Bool = false, continueOperation: Bool = false, abortOperation: Bool = false,
        stageResolved: Bool = false, push: Bool = false, pushReconcile: Bool = false
    ) {
        self.stage = stage
        self.unstage = unstage
        self.commit = commit
        self.branches = branches
        self.createBranch = createBranch
        self.switchBranch = switchBranch
        self.sync = sync
        self.fetch = fetch
        self.fastForward = fastForward
        self.merge = merge
        self.rebase = rebase
        self.continueOperation = continueOperation
        self.abortOperation = abortOperation
        self.stageResolved = stageResolved
        self.push = push
        self.pushReconcile = pushReconcile
    }

    public static let none = GitMutationSupport()
    /// Every slice-2 change (staging, commit, branches).
    public static let all = GitMutationSupport(
        stage: true, unstage: true, commit: true, branches: true, createBranch: true, switchBranch: true
    )
    /// Every change this build knows, slice 3 included.
    public static let everything = GitMutationSupport(
        stage: true, unstage: true, commit: true, branches: true, createBranch: true, switchBranch: true,
        sync: true, fetch: true, fastForward: true, merge: true, rebase: true,
        continueOperation: true, abortOperation: true, stageResolved: true
    )
    /// Every change this build knows, push and "Check the remote" included.
    public static let withPush = GitMutationSupport(
        stage: true, unstage: true, commit: true, branches: true, createBranch: true, switchBranch: true,
        sync: true, fetch: true, fastForward: true, merge: true, rebase: true,
        continueOperation: true, abortOperation: true, stageResolved: true, push: true, pushReconcile: true
    )

    public init(endpoints: GatewayEndpoints?) {
        guard let endpoints, endpoints.gitReceipts else {
            self = .none
            return
        }
        self.init(
            stage: endpoints.gitStage,
            unstage: endpoints.gitUnstage,
            commit: endpoints.gitCommit,
            branches: endpoints.gitBranches,
            createBranch: endpoints.gitBranches && endpoints.gitCreateBranch,
            switchBranch: endpoints.gitBranches && endpoints.gitSwitchBranch,
            sync: endpoints.gitSync,
            fetch: endpoints.gitFetch,
            fastForward: endpoints.gitFastForward,
            merge: endpoints.gitMerge,
            rebase: endpoints.gitRebase,
            continueOperation: endpoints.gitContinue,
            abortOperation: endpoints.gitAbort,
            stageResolved: endpoints.gitStage && endpoints.gitStageResolved,
            push: endpoints.gitPushPreview && endpoints.gitPush,
            pushReconcile: endpoints.gitPushPreview && endpoints.gitPush && endpoints.gitPushReconcile
        )
    }

    public var any: Bool {
        stage || unstage || commit || branches || fetch || fastForward || merge || rebase
            || continueOperation || abortOperation || push
    }

    /// Whether any way of bringing incoming commits in is served.
    public var updates: Bool { fastForward || merge || rebase }
}

// MARK: - Phase

/// Where the one change in flight for a repository stands.
///
/// Four of these are outcomes the person sees: completed, rejected (which
/// includes refresh required), refused before the Mac accepted anything,
/// and uncertain. `lost` is the phone's own uncertainty after an answer did
/// not arrive; it lasts until the receipt on the Mac says what happened.
public enum GitMutationPhase: Equatable, Sendable {
    case idle
    case sending(GitMutationAction)
    /// The Mac accepted it and is still running it.
    case pending(GitMutationAction, GitMutationReceipt)
    case completed(GitMutationAction, GitMutationReceipt)
    case rejected(GitMutationAction, GitMutationReceipt)
    /// The Mac refused the request before accepting it, so nothing ran.
    case refused(GitMutationAction, code: String, message: String)
    /// The Mac itself cannot tell whether Git finished.
    case uncertain(GitMutationAction, GitMutationReceipt)
    /// The answer was lost on the way back; nothing is known yet.
    case lost(GitMutationAction)
    /// Reading the receipt after a lost answer.
    case reconciling(GitMutationAction)
    /// The person asked the Mac to check the remote for a push it could
    /// not confirm (`POST /v2/git/push/reconcile`). The receipt is the
    /// one being checked.
    case checkingRemote(GitMutationAction, GitMutationReceipt)

    public var action: GitMutationAction? {
        switch self {
        case .idle: nil
        case .sending(let action), .lost(let action), .reconciling(let action): action
        case .pending(let action, _), .completed(let action, _), .rejected(let action, _),
             .uncertain(let action, _), .checkingRemote(let action, _): action
        case .refused(let action, _, _): action
        }
    }

    public var receipt: GitMutationReceipt? {
        switch self {
        case .pending(_, let receipt), .completed(_, let receipt), .rejected(_, let receipt),
             .uncertain(_, let receipt), .checkingRemote(_, let receipt): receipt
        default: nil
        }
    }

    /// While true, no other change may start: one is running, or its
    /// outcome is not yet known or not yet seen.
    public var blocksNewActions: Bool {
        switch self {
        case .idle, .completed, .rejected, .refused: false
        case .sending, .pending, .uncertain, .lost, .reconciling, .checkingRemote: true
        }
    }

    public var isWorking: Bool {
        switch self {
        case .sending, .pending, .reconciling, .checkingRemote: true
        default: false
        }
    }

    /// A rejection because the repository changed after it was shown.
    public var isRefreshRequired: Bool {
        if case .rejected(_, let receipt) = self { return receipt.outcome?.code == "refresh_required" }
        return false
    }

    /// A push the Mac could not confirm, or one still running, whose
    /// result only the remote can now settle.
    public var unconfirmedPush: (GitMutationAction, GitMutationReceipt)? {
        switch self {
        case .uncertain(let action, let receipt), .pending(let action, let receipt):
            action.kind == .push ? (action, receipt) : nil
        default: nil
        }
    }
}

// MARK: - Runner

/// Sends at most one change at a time for one repository, and owns the
/// lost-answer rule: after a transport failure nothing new may start until
/// the receipt on the Mac has been read. A receipt the Mac never recorded
/// means the request never arrived, and the same request — same id — is
/// sent again; the Mac's deduplication makes that safe. A new request id is
/// only ever minted for a new action the person chose.
@MainActor
@Observable
public final class GitMutationRunner {
    public typealias Sending = (GitMutationAction) async throws -> GitMutationReceipt
    public typealias Lookup = (String) async throws -> GitMutationReceipt
    /// `POST /v2/git/push/reconcile` for one request id.
    public typealias RemoteCheck = (String) async throws -> GitPushReconciliation

    public private(set) var phase: GitMutationPhase = .idle {
        didSet {
            if let receipt = phase.receipt { lastReceipt = receipt }
            if phase.action?.requestId != oldValue.action?.requestId, phase.receipt == nil {
                lastReceipt = nil
            }
            if let id = phase.action?.requestId, id != remoteCheckRequestID,
               id != unresolvedPush?.action.requestId {
                remoteCheck = nil
                remoteCheckNote = nil
                remoteCheckRequestID = nil
            }
        }
    }
    /// What the Mac said the last time it checked the remote for the push
    /// in ``phase`` (or the parked one), and the phone's own note when the
    /// check could not be made. Never a reason to call the push not run.
    public private(set) var remoteCheck: GitPushReconciliation?
    public private(set) var remoteCheckNote: String?
    private var remoteCheckRequestID: String?
    /// A push whose uncertain outcome the person put aside. Other changes
    /// may start, but no new push until the remote has been checked and
    /// the receipt says what happened.
    public private(set) var unresolvedPush: (action: GitMutationAction, receipt: GitMutationReceipt)?
    /// Whether this Mac can check the remote (`gitPushReconcile`).
    var checkRemoteRoute: RemoteCheck?
    public var canCheckRemote: Bool { checkRemoteRoute != nil }
    /// The latest receipt read for the change in ``phase``, kept through a
    /// lost answer so an uncertain outcome can say what Latch last knew.
    public private(set) var lastReceipt: GitMutationReceipt?
    /// Whether a loop is reading a pending receipt right now.
    public private(set) var isPolling = false
    /// When the transport failed for the change in ``phase``.
    public private(set) var connectionLostAt: Date?
    /// While a progress view is on screen the receipt is read about once a
    /// second; otherwise the reads back off. Either way they continue until
    /// the receipt settles: "Stop waiting" never stops Git or the check.
    public var isWatchingProgress = true

    private let send: Sending
    private let lookup: Lookup
    private let isOnline: () -> Bool
    private let sleep: (Double) async -> Void
    /// Runs after every outcome that may have changed the repository.
    var afterSettled: (@MainActor (GitMutationAction) async -> Void)?
    private var followUp: (@MainActor () async -> Void)?
    /// Bumped whenever a new loop starts reading a receipt, so an older one
    /// that is still asleep stops instead of reading alongside it.
    private var pollGeneration = 0

    /// Waits between receipt reads while the Mac is still running a change.
    static let pollDelays: [Double] = [0.5, 1, 1, 2, 2, 3, 3, 5, 5, 8]
    /// Fetch, merge and rebase may legitimately run for minutes (the Mac
    /// kills Git at its own deadline and settles the receipt), so these are
    /// read for much longer than a commit before the phone leaves the
    /// receipt pending and checks again on return or reconnect.
    static let longRunningWatchingDelay: Double = 1
    static let longRunningBackoff: [Double] = [1, 2, 3, 5, 8, 10]
    static let longRunningPollBudget: Double = 15 * 60
    private let now: () -> Date

    public init(
        send: @escaping Sending,
        lookup: @escaping Lookup,
        isOnline: @escaping () -> Bool = { true },
        sleep: @escaping (Double) async -> Void = { seconds in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
        },
        now: @escaping () -> Date = { Date() }
    ) {
        self.send = send
        self.lookup = lookup
        self.isOnline = isOnline
        self.sleep = sleep
        self.now = now
    }

    public var canStart: Bool { isOnline() && !phase.blocksNewActions }

    /// Sends a new action. Ignored while another change blocks it.
    public func perform(_ action: GitMutationAction, then followUp: (@MainActor () async -> Void)? = nil) async {
        guard canStart else { return }
        self.followUp = followUp
        isWatchingProgress = true
        await transmit(action)
    }

    /// Clears a seen outcome. An uncertain one is cleared only by reading
    /// the repository again, which the caller does through `afterSettled`.
    /// An uncertain push, on a Mac that can check the remote, is put aside
    /// rather than forgotten: no new push starts until it is resolved.
    public func acknowledge() {
        switch phase {
        case .uncertain(let action, let receipt):
            if action.kind == .push {
                unresolvedPush = (action, receipt)
            }
            phase = .idle
        case .completed, .rejected, .refused: phase = .idle
        default: break
        }
    }

    /// Whether "Check the remote" applies now: a push the Mac could not
    /// confirm, one still pending after the link dropped or the reads
    /// stopped, or one the person put aside.
    public var offersRemoteCheck: Bool {
        guard canCheckRemote, isOnline() else { return false }
        switch phase {
        case .uncertain(let action, _): return action.kind == .push
        case .pending(let action, _): return action.kind == .push && (!isPolling || connectionLostAt != nil)
        case .idle, .completed, .rejected, .refused: return unresolvedPush != nil
        default: return false
        }
    }

    /// Asks the Mac to look at the remote for a push it could not confirm.
    /// The receipt settles only on the Mac's evidence; when the remote
    /// can't be reached it stays uncertain and the Mac's message is shown.
    /// If Git is still running for that push, the phone keeps waiting.
    public func checkRemote() async {
        guard let check = checkRemoteRoute, isOnline() else { return }
        let action: GitMutationAction
        let receipt: GitMutationReceipt
        if let (unconfirmed, current) = phase.unconfirmedPush {
            action = unconfirmed
            receipt = current
        } else if !phase.blocksNewActions, let parked = unresolvedPush {
            action = parked.action
            receipt = parked.receipt
        } else {
            return
        }
        let previous = phase.unconfirmedPush == nil ? GitMutationPhase.uncertain(action, receipt) : phase
        // Any loop still reading the pending receipt stops here.
        pollGeneration += 1
        isPolling = false
        // While checking, and after, the outcome is on screen again; seeing
        // it still uncertain and closing it puts it aside once more.
        unresolvedPush = nil
        phase = .checkingRemote(action, receipt)
        remoteCheckRequestID = action.requestId
        remoteCheckNote = nil
        do {
            let result = try await check(action.requestId)
            remoteCheck = result
            if result.receipt.state == .uncertain {
                remoteCheckNote = result.message
            }
            await settle(action, result.receipt)
        } catch LatchError.git(code: "still_running", let reason) {
            // Git is still running that push on the Mac: wait for it.
            remoteCheckNote = reason.isEmpty
                ? "Git is still running this push on your Mac. Latch keeps checking."
                : reason
            phase = .pending(action, receipt)
            await reconcile()
        } catch {
            // The check itself failed. Nothing is known that wasn't before,
            // and nothing is inferred: the push stays as it was.
            remoteCheckNote = "Latch couldn't check the remote: \(gitMessage(for: error))"
            phase = previous
        }
    }

    /// After a lost answer or a still-running change, asks the Mac what
    /// became of it. Called when the link returns and from "Check again".
    public func reconcile() async {
        let action: GitMutationAction
        switch phase {
        case .lost(let lost): action = lost
        case .pending(let pending, _): action = pending
        default: return
        }
        // A loop is already reading this receipt; a second one would only
        // double the reads.
        guard isOnline(), !isPolling else { return }
        phase = .reconciling(action)
        do {
            let receipt = try await lookup(action.requestId)
            await settle(action, receipt)
        } catch LatchError.git(code: "receipt_not_found", _) {
            // The Mac never accepted it, so it never ran. The same request,
            // under the same id, is the safe way to try again.
            await transmit(action)
        } catch {
            markLost(action)
        }
    }

    private func markLost(_ action: GitMutationAction) {
        connectionLostAt = now()
        phase = .lost(action)
    }

    private func transmit(_ action: GitMutationAction) async {
        connectionLostAt = nil
        phase = .sending(action)
        do {
            let receipt = try await send(action)
            await settle(action, receipt)
        } catch let error as LatchError {
            switch error {
            case .git(let code, _) where code == "receipt_unavailable" || code == "response_too_large":
                // These failures can happen after acceptance/execution.
                // Read the existing receipt; never treat them as not run.
                markLost(action)
            case .git(let code, let reason):
                phase = .refused(action, code: code, message: reason)
            case .refused(let reason):
                phase = .refused(action, code: "refused", message: reason)
            case .unauthorized:
                phase = .refused(action, code: "unauthorized", message: error.message)
            case .endpointUnavailable:
                phase = .refused(action, code: "unavailable", message: error.message)
            default:
                // A transport failure, or an answer that did not parse: the
                // Mac may have done it. Only its receipt can say.
                markLost(action)
            }
        } catch {
            markLost(action)
        }
    }

    /// The wait before the next read of a pending receipt, or nil once the
    /// phone should stop reading for now and leave it pending.
    private func nextDelay(_ action: GitMutationAction, attempt: Int, waited: Double) -> Double? {
        guard action.isLongRunning else {
            return attempt < Self.pollDelays.count ? Self.pollDelays[attempt] : nil
        }
        guard waited < Self.longRunningPollBudget else { return nil }
        if isWatchingProgress { return Self.longRunningWatchingDelay }
        return Self.longRunningBackoff[min(attempt, Self.longRunningBackoff.count - 1)]
    }

    private func settle(_ action: GitMutationAction, _ first: GitMutationReceipt) async {
        var receipt = first
        var attempt = 0
        var waited: Double = 0
        pollGeneration += 1
        let generation = pollGeneration
        isPolling = true
        defer { if generation == pollGeneration { isPolling = false } }
        while receipt.state == .pending {
            phase = .pending(action, receipt)
            guard let delay = nextDelay(action, attempt: attempt, waited: waited), isOnline() else { return }
            attempt += 1
            waited += delay
            await sleep(delay)
            // Another loop, or a check of the remote, took over.
            guard generation == pollGeneration else { return }
            // The link may have gone while waiting; the receipt is read again
            // on reconnect rather than reported lost.
            guard isOnline() else { return }
            do {
                receipt = try await lookup(action.requestId)
            } catch {
                guard generation == pollGeneration else { return }
                markLost(action)
                return
            }
            guard generation == pollGeneration else { return }
        }
        isPolling = false
        switch receipt.state {
        case .completed: phase = .completed(action, receipt)
        case .rejected: phase = .rejected(action, receipt)
        case .uncertain, .pending: phase = .uncertain(action, receipt)
        }
        await afterSettled?(action)
        if let followUp {
            self.followUp = nil
            await followUp()
        }
    }
}

// MARK: - Branches sheet

/// The Branches sheet: local branches as the Mac lists them, and the
/// create/switch actions, which run through the repository's one runner.
@MainActor
@Observable
public final class GitBranchesModel {
    public typealias Listing = (String) async throws -> GitBranchList

    public let root: String
    public private(set) var list: GitBranchList?
    public private(set) var isLoading = false
    public private(set) var error: String?
    public var query = ""

    public let runner: GitMutationRunner
    public let support: GitMutationSupport
    private let listing: Listing
    private let makeRequestID: () -> String

    init(
        root: String, runner: GitMutationRunner, support: GitMutationSupport,
        listing: @escaping Listing, makeRequestID: @escaping () -> String
    ) {
        self.root = root
        self.runner = runner
        self.support = support
        self.listing = listing
        self.makeRequestID = makeRequestID
    }

    public func load() async {
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            list = try await listing(root)
            error = nil
        } catch {
            self.error = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    public var current: GitBranch? { list?.branches.first(where: \.current) }

    /// The others, filtered by the search field; the current one is shown
    /// on its own at the top.
    public var others: [GitBranch] {
        let rest = (list?.branches ?? []).filter { !$0.current }
        let needle = query.trimmingCharacters(in: .whitespaces)
        guard !needle.isEmpty else { return rest }
        return rest.filter { $0.name.localizedCaseInsensitiveContains(needle) }
    }

    /// What a new branch starts from: the current branch, or HEAD's commit
    /// when detached. Nil on an unborn branch, which has nothing to start at.
    public var startPoint: (name: String, oid: String)? {
        guard let list, let oid = list.head.oid else { return nil }
        return (list.head.branch ?? "HEAD", oid)
    }

    public func canSwitch(to branch: GitBranch) -> Bool {
        support.switchBranch && !branch.current && !branch.checkedOutElsewhere
            && list != nil && runner.canStart
    }

    public var canCreate: Bool {
        support.createBranch && startPoint != nil && runner.canStart
    }

    public func switchBranch(to branch: GitBranch) async {
        guard let list, canSwitch(to: branch) else { return }
        await runner.perform(.switchBranch(GitSwitchBranchRequest(
            requestId: makeRequestID(), path: root, expected: list.preconditions,
            name: branch.name, oid: branch.oid
        ))) { [weak self] in await self?.load() }
    }

    public func createBranch(named name: String, switchToIt: Bool) async {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let list, let start = startPoint, canCreate, GitBranchName.isPlausible(name) else { return }
        await runner.perform(.createBranch(GitCreateBranchRequest(
            requestId: makeRequestID(), path: root, expected: list.preconditions,
            name: name, startPoint: start.name, startOid: start.oid, switch: switchToIt
        ))) { [weak self] in await self?.load() }
    }
}

/// A first check of a branch name, so the button stays off for something
/// Git will certainly refuse. Git on the Mac is the authority.
public enum GitBranchName {
    public static func isPlausible(_ name: String) -> Bool {
        guard !name.isEmpty, name.utf8.count <= 255, !name.hasPrefix("-"), !name.hasPrefix("/"),
              !name.hasSuffix("/"), !name.hasSuffix("."), !name.hasSuffix(".lock"),
              !name.contains(".."), !name.contains("@{"), !name.contains("//"), name != "@"
        else { return false }
        let forbidden = CharacterSet(charactersIn: " ~^:?*[\\").union(.controlCharacters)
        return name.unicodeScalars.allSatisfy { !forbidden.contains($0) }
    }
}
