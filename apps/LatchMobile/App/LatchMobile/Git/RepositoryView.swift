import LatchMobileKit
import SwiftUI
import UIKit

/// One repository on the Mac: where it is, what branch, and what changed.
///
/// It is a pushed screen with the session screens' floating chrome, and it
/// never attaches to a terminal. Staging, Commit, Branches, Fetch, Sync and
/// Continue/Abort change the repository through the Mac's receipts, one
/// change at a time.
struct RepositoryView: View {
    /// Any folder inside the working tree; the Mac resolves it.
    let folder: String
    /// A model already read, for previews; the screen otherwise builds its
    /// own after the owner check.
    var preloaded: GitRepositoryModel?

    @Environment(AppModel.self) private var appModel
    @Environment(\.openSession) private var openSession
    @State private var repository: GitRepositoryModel?
    @State private var isOpening = true
    @State private var openFailure: String?
    @State private var diffEntry: GitStatusEntry?
    @State private var sheet: RepositorySheet?
    @State private var blockedReason: String?
    @State private var toast: String?
    /// An outcome sheet the person closed, so it is not raised again.
    @State private var hiddenOutcome: String?
    /// Remotes to choose from when the branch tracks none.
    @State private var remoteChoices: [String] = []
    @State private var confirmingAbort = false

    enum RepositorySheet: Identifiable {
        case details
        case commit
        case branches(GitBranchesModel)
        /// The Sync sheet, opened on the push review when one is given.
        case sync(GitSyncModel, GitPushModel?)

        var id: String {
            switch self {
            case .details: "details"
            case .commit: "commit"
            case .branches: "branches"
            case .sync: "sync"
            }
        }
    }

    private var status: GitStatus? { repository?.status }

    var body: some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) {
                SessionChromeBar(
                    title: status?.name ?? FolderPickerView.displayName(of: folder),
                    status: status?.chromeStatus,
                    statusIdentifier: "repository.chrome.status",
                    showDetails: { openBranches() }
                ) {
                    moreMenu
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if status != nil {
                    VStack(spacing: 8) {
                        if let toast {
                            GitToast(text: toast)
                                .padding(.horizontal, 16)
                                .transition(.move(edge: .bottom).combined(with: .opacity))
                        }
                        bottomBar
                    }
                    .animation(.default, value: toast)
                }
            }
            .toolbar(.hidden, for: .navigationBar)
            .background(InteractivePopRestorer())
            .navigationDestination(item: $diffEntry) { entry in
                if let repository {
                    GitDiffView(repository: repository, entry: entry)
                }
            }
            .sheet(item: $sheet) { sheet in
                switch sheet {
                case .details:
                    if let status { RepositoryDetailsSheet(status: status) }
                case .commit:
                    if let repository { CommitSheet(repository: repository) }
                case .branches(let model):
                    if let repository {
                        BranchesSheet(repository: repository, model: model) {
                            reopen(.commit)
                        }
                    }
                case .sync(let model, let push):
                    if let repository { SyncSheet(repository: repository, model: model, push: push) }
                }
            }
            .sheet(isPresented: resultSheetShown) {
                if let repository {
                    ScrollView {
                        if let progress = repository.progress {
                            GitProgressCard(
                                progress: progress,
                                stopWaiting: { repository.stopWaiting() },
                                checkRemote: repository.offersRemoteCheck
                                    ? { Task { await repository.checkRemote() } } : nil
                            )
                            .padding(16)
                        } else if let presentation = repositoryOutcome {
                            GitOutcomeCard(
                                presentation: presentation,
                                primary: { Task { await perform(presentation.nextStep) } },
                                dismiss: { hiddenOutcome = outcomeKey },
                                secondary: presentation.secondaryStep.map { step in
                                    { Task { await perform(step) } }
                                }
                            )
                            .padding(16)
                        }
                    }
                    .presentationDetents([.medium, .large])
                }
            }
            .alert(
                "This phone can't do that right now",
                isPresented: Binding(get: { blockedReason != nil }, set: { if !$0 { blockedReason = nil } })
            ) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(blockedReason ?? "")
            }
            .confirmationDialog(
                "Fetch from which remote?",
                isPresented: Binding(get: { !remoteChoices.isEmpty }, set: { if !$0 { remoteChoices = [] } }),
                titleVisibility: .visible
            ) {
                ForEach(remoteChoices, id: \.self) { remote in
                    Button(remote) { Task { await repository?.fetch(remote: remote) } }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This branch doesn't track a remote branch, so choose which remote to fetch.")
            }
            .confirmationDialog(
                repository?.conflictMarkersQuestion ?? "",
                isPresented: conflictMarkersShown,
                titleVisibility: .visible
            ) {
                Button("Stage anyway") { Task { await repository?.confirmConflictMarkers() } }
                Button("Cancel", role: .cancel) { repository?.declineConflictMarkers() }
            } message: {
                Text("Staging marks the file resolved. Latch doesn't edit files; resolve them on your Mac or in a terminal session.")
            }
            .confirmationDialog(
                repository?.operationState.map { "\($0.abortLabel)?" } ?? "Abort?",
                isPresented: $confirmingAbort,
                titleVisibility: .visible
            ) {
                Button(repository?.operationState?.abortLabel ?? "Abort", role: .destructive) {
                    Task { await repository?.abortOperation() }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(repository?.operationState?.abortConfirmation ?? "")
            }
            .onChange(of: repository?.mutations.phase) { _, phase in
                guard case .completed(_, let receipt)? = phase, receipt.outcome?.code != "stopped" else { return }
                let message = receipt.outcome?.message ?? "Done."
                toast = message
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 3_000_000_000)
                    if toast == message {
                        toast = nil
                        if case .completed? = repository?.mutations.phase { repository?.mutations.acknowledge() }
                    }
                }
            }
            .task {
                guard repository == nil else { return }
                if let preloaded {
                    repository = preloaded
                    isOpening = false
                    return
                }
                repository = await appModel.gitRepository(folder: folder)
                openFailure = appModel.ownerAuthenticationFailure
                isOpening = false
            }
            // Back on the link: read the Mac again rather than keep showing
            // what it said before the interruption.
            .onChange(of: appModel.linkState.isUsable) { _, usable in
                guard usable else { return }
                // A change whose answer was lost, or one still running, is
                // settled from its receipt before anything else can start.
                Task { await repository?.reconnected() }
            }
    }

    @ViewBuilder
    private var content: some View {
        if let repository, let status = repository.status {
            loaded(repository, status: status)
        } else if let repository, let error = repository.error {
            VStack(spacing: 12) {
                MessageView(icon: "exclamationmark.triangle", title: "Cannot open this repository", detail: error)
                Button("Try again") { Task { await repository.refresh() } }
                    .buttonStyle(.bordered)
                    .padding(.bottom, 24)
            }
        } else if isOpening || repository != nil {
            ProgressView(isOpening ? "Checking…" : "Reading the repository…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            MessageView(
                icon: "lock",
                title: "Not unlocked",
                detail: openFailure ?? Self.deniedDetail
            )
        }
    }

    private func loaded(_ repository: GitRepositoryModel, status: GitStatus) -> some View {
        let stale = repository.isStale
        return List {
            if stale {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    GitStalePill(text: repository.stalePill(now: context.date))
                }
                .frame(maxWidth: .infinity)
                .plainRow()
            }
            Group {
                RepositoryHeaderCard(
                    status: status, fetch: fetchControl(repository),
                    reviewPush: repository.offersPush && repository.outgoingCount > 0 ? { openPush() } : nil
                )
                    .plainRow()
                if !status.sessions.isEmpty {
                    SessionsWorkingHereCard(sessions: status.sessions, open: openSession)
                        .plainRow()
                }
                if let state = repository.operationState {
                    OperationCard(state: state, branch: status.head.branch)
                        .plainRow()
                }
                if let error = repository.error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .plainRow()
                }
                if status.entries.isEmpty {
                    Label("Nothing to commit. The working tree matches HEAD.", systemImage: "checkmark.circle")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 24)
                        .frame(maxWidth: .infinity)
                        .plainRow()
                }
                ForEach(repository.sections) { group in
                    Section {
                        ForEach(group.entries) { entry in
                            GitChangeRow(
                                entry: entry,
                                staging: repository.offersStaging(entry)
                                    ? GitChangeRow.Staging(
                                        enabled: repository.canToggleStaging(entry),
                                        toggle: { toggle(entry) }
                                    )
                                    : nil
                            ) { diffEntry = entry }
                        }
                    } header: {
                        sectionHeader(group)
                    }
                }
                if status.entriesTruncated {
                    Text("Showing \(status.entries.count) of \(status.totalEntries) changes. Review the rest on your Mac.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .plainRow()
                }
            }
            // Stale content stays readable but steps back, so nobody acts on
            // it as if it were live.
            .opacity(stale ? 0.55 : 1)
        }
        .listStyle(.plain)
        .refreshable { await repository.refresh() }
    }

    private func sectionHeader(_ group: GitSectionGroup) -> some View {
        HStack {
            Text("\(group.section.title) \(group.entries.count)")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.primary)
            Spacer()
            if let repository, repository.offersSectionAction(group.section) {
                let title = group.section == .staged ? "Unstage all" : "Stage all"
                Button(title) {
                    guard repository.canMutate else {
                        blockedReason = repository.mutationBlockReason
                        return
                    }
                    Task { await repository.applyToSection(group.section) }
                }
                .font(.footnote.weight(.semibold))
                .opacity(repository.canMutate ? 1 : 0.4)
                .accessibilityLabel("\(title) in \(group.section.title)")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isHeader)
    }

    private func toggle(_ entry: GitStatusEntry) {
        guard let repository else { return }
        guard repository.canToggleStaging(entry) else {
            blockedReason = repository.mutationBlockReason
            return
        }
        Task { await repository.toggleStaging(entry) }
    }

    /// Opens a sheet once the one on screen has gone.
    private func reopen(_ next: RepositorySheet) {
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 400_000_000)
            self.sheet = next
        }
    }

    private func openBranches() {
        guard let repository else { return }
        if let model = repository.branchesModel() {
            sheet = .branches(model)
        } else {
            sheet = .details
        }
    }

    private func openCommit() {
        guard let repository else { return }
        if let reason = repository.commitBlockReason {
            blockedReason = reason
            return
        }
        sheet = .commit
    }

    private func openSync(choosingMergeOrRebase: Bool = false) {
        guard let repository else { return }
        if let reason = repository.syncBlockReason {
            blockedReason = reason
            return
        }
        guard let model = repository.syncModel() else { return }
        if choosingMergeOrRebase { model.chooseMergeOrRebase() }
        sheet = .sync(model, nil)
    }

    /// The Sync sheet opened on the push review.
    private func openPush() {
        guard let repository else { return }
        if let reason = repository.syncBlockReason ?? repository.pushBlockReason {
            blockedReason = reason
            return
        }
        guard let model = repository.syncModel(), let push = repository.pushModel() else { return }
        sheet = .sync(model, push)
    }

    // MARK: Fetch

    /// The remote the header's Fetch names, for its VoiceOver label.
    private var fetchRemoteHint: String? {
        if let remote = repository?.syncPreview?.upstream?.remote { return remote }
        guard let upstream = status?.upstream?.name, let slash = upstream.firstIndex(of: "/") else { return nil }
        return String(upstream[..<slash])
    }

    private func fetchControl(_ repository: GitRepositoryModel) -> RepositoryHeaderCard.FetchControl? {
        guard repository.offersFetch else { return nil }
        let working: Bool = if case .fetch? = repository.mutations.phase.action?.kind { repository.mutations.phase.isWorking } else { false }
        return RepositoryHeaderCard.FetchControl(
            enabled: repository.canFetch,
            working: working,
            label: fetchRemoteHint.map { "Fetch \($0)" } ?? "Fetch",
            hint: repository.fetchBlockReason ?? "Updates what your Mac knows about the remote. Nothing is merged.",
            action: startFetch
        )
    }

    private func startFetch() {
        guard let repository else { return }
        guard repository.canFetch else {
            blockedReason = repository.fetchBlockReason
            return
        }
        Task {
            let remotes = await repository.fetchRemotes()
            switch remotes.count {
            case 0: blockedReason = "This repository has no remotes to fetch from."
            case 1: await repository.fetch(remote: remotes[0])
            default: remoteChoices = remotes
            }
        }
    }

    // MARK: Outcomes and progress

    private func perform(_ step: GitOutcomePresentation.NextStep) async {
        guard let repository else { return }
        // "Fetch and choose how to update" after the remote refused a push
        // that wasn't a fast-forward: Sync, fetching that remote.
        if step == .fetchAndChooseUpdate, case .push(let request)? = repository.mutations.phase.action {
            await repository.acknowledgeOutcome()
            try? await Task.sleep(nanoseconds: 400_000_000)
            openSync()
            if case .sync(let model, _)? = sheet {
                await model.load()
                if model.canFetch, model.fetchRemotes.contains(request.remote) {
                    await model.fetch(remote: request.remote)
                }
            }
            return
        }
        await repository.perform(
            step,
            openCommit: { reopen(.commit) },
            openSync: {
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 400_000_000)
                    openSync(choosingMergeOrRebase: step == .chooseMergeOrRebase)
                }
            },
            openPush: {
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 400_000_000)
                    openPush()
                }
            }
        )
    }

    /// The repository-level outcome card: only when no sheet of its own is
    /// up and the diff screen is not showing one. A conflict-marker refusal
    /// asks its question instead.
    private var repositoryOutcome: GitOutcomePresentation? {
        guard let repository, repository.conflictMarkersQuestion == nil,
              let presentation = repository.outcomePresentation,
              presentation.style != .completed
        else { return nil }
        return presentation
    }

    private var outcomeKey: String? {
        guard let repository, let action = repository.mutations.phase.action,
              let presentation = repositoryOutcome else { return nil }
        return "\(action.requestId)·\(presentation.headline)"
    }

    /// Progress while a long change runs (until "Stop waiting"), otherwise
    /// the outcome, in one sheet over the repository.
    private var resultSheetShown: Binding<Bool> {
        Binding(
            get: {
                guard sheet == nil, diffEntry == nil, let repository else { return false }
                if repository.progress != nil { return true }
                return repositoryOutcome != nil && hiddenOutcome != outcomeKey
            },
            set: { shown in
                guard !shown, let repository else { return }
                // Swiping the progress away is "Stop waiting": Git goes on.
                if repository.progress != nil {
                    repository.stopWaiting()
                } else {
                    hiddenOutcome = outcomeKey
                }
            }
        )
    }

    private var conflictMarkersShown: Binding<Bool> {
        Binding(
            get: { sheet == nil && diffEntry == nil && repository?.conflictMarkersQuestion != nil },
            set: { shown in if !shown, repository?.conflictMarkersQuestion != nil { repository?.declineConflictMarkers() } }
        )
    }

    private var moreMenu: some View {
        Menu {
            Button {
                Task { await repository?.refresh() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(repository == nil || !appModel.linkState.isUsable)
            if repository?.offersFetch == true {
                Button {
                    startFetch()
                } label: {
                    Label(fetchRemoteHint.map { "Fetch \($0)" } ?? "Fetch", systemImage: "arrow.down.circle")
                }
            }
            if repository?.offersSync == true {
                Button {
                    openSync()
                } label: {
                    Label("Sync…", systemImage: "arrow.triangle.2.circlepath")
                }
            }
            if repository?.offersPush == true {
                Button {
                    openPush()
                } label: {
                    Label("Review push…", systemImage: "arrow.up.circle")
                }
            }
            if repository?.offersBranches == true {
                Button {
                    openBranches()
                } label: {
                    Label("Branches…", systemImage: "arrow.triangle.branch")
                }
            }
            Button {
                sheet = .details
            } label: {
                Label("Details", systemImage: "info.circle")
            }
            .disabled(status == nil)
            if let root = status?.root {
                Button {
                    UIPasteboard.general.string = root
                } label: {
                    Label("Copy path", systemImage: "doc.on.doc")
                }
            }
        } label: {
            FloatingControl(systemImage: "ellipsis")
                .foregroundStyle(.primary)
        }
        .tint(.primary)
        .accessibilityLabel("More")
        .accessibilityIdentifier("repository.chrome.more")
    }

    // MARK: Bottom bar

    /// Commit and Sync; Continue and Abort… while a merge or rebase is in
    /// progress.
    private var bottomBar: some View {
        VStack(spacing: 6) {
            if let repository, let state = repository.operationState,
               repository.offersContinue || repository.offersAbort {
                operationControls(repository, state: state)
            } else {
                changeControls
            }
            if repository?.isRunningInBackground == true {
                Button {
                    Task { await repository?.resumeWaiting() }
                } label: {
                    Label("Git is still running on your Mac · Show progress", systemImage: "hourglass")
                        .font(.caption.weight(.semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.accentColor)
                .accessibilityIdentifier("repository.showProgress")
            }
            Text(caption)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background { BottomFade() }
    }

    private var changeControls: some View {
        let staged = status?.entries(in: .staged).count ?? 0
        let arrows = GitPresentation.aheadBehindArrows(status?.upstream)
        let offersCommit = repository?.offersCommit == true
        let canCommit = repository?.canCommit == true
        let offersSync = repository?.offersSync == true
        let canSync = offersSync && repository?.syncBlockReason == nil
        return HStack(spacing: 12) {
            if offersCommit {
                Button {
                    openCommit()
                } label: {
                    FloatingControl(shape: .capsule, size: 48, tint: canCommit ? .accentColor : nil) {
                        Label(staged > 0 ? "Commit \(staged)" : "Commit", systemImage: "checkmark")
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                    }
                    .foregroundStyle(canCommit ? AnyShapeStyle(.white) : AnyShapeStyle(.tertiary))
                }
                .accessibilityHint(repository?.commitBlockReason ?? "Reviews the staged files before committing")
                .accessibilityIdentifier("repository.commit")
            }
            if offersSync {
                Button {
                    openSync()
                } label: {
                    FloatingControl(shape: .capsule, size: 48) {
                        Label(arrows.map { "Sync \($0)" } ?? "Sync", systemImage: "arrow.triangle.2.circlepath")
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                    }
                    .foregroundStyle(canSync ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
                }
                .accessibilityLabel("Sync")
                .accessibilityValue(GitPresentation.aheadBehindSpoken(status?.upstream))
                .accessibilityHint(repository?.syncBlockReason ?? "Reviews incoming commits and how to bring them in")
                .accessibilityIdentifier("repository.sync")
            }
        }
        .buttonStyle(.plain)
    }

    private func operationControls(_ repository: GitRepositoryModel, state: GitOperationState) -> some View {
        let canContinue = repository.canContinue
        let canAbort = repository.canAbort
        return HStack(spacing: 12) {
            if repository.offersContinue {
                Button {
                    guard canContinue else {
                        blockedReason = repository.continueBlockReason
                        return
                    }
                    Task { await repository.continueOperation() }
                } label: {
                    FloatingControl(shape: .capsule, size: 48, tint: canContinue ? .accentColor : nil) {
                        Label("Continue", systemImage: "play.fill")
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                    }
                    .foregroundStyle(canContinue ? AnyShapeStyle(.white) : AnyShapeStyle(.tertiary))
                }
                .accessibilityLabel(state.continueLabel)
                .accessibilityHint(repository.continueBlockReason ?? "Runs the next step on your Mac")
                .accessibilityIdentifier("repository.continue")
            }
            if repository.offersAbort {
                Button {
                    guard canAbort else {
                        blockedReason = repository.mutationBlockReason
                        return
                    }
                    confirmingAbort = true
                } label: {
                    FloatingControl(shape: .capsule, size: 48) {
                        Label("Abort…", systemImage: "xmark")
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                    }
                    .foregroundStyle(canAbort ? AnyShapeStyle(Color.red) : AnyShapeStyle(.tertiary))
                }
                .accessibilityLabel(state.abortLabel)
                .accessibilityHint(repository.mutationBlockReason ?? "Asks before putting the branch back")
                .accessibilityIdentifier("repository.abort")
            }
        }
        .buttonStyle(.plain)
    }

    private var caption: String {
        guard let repository, repository.offersChanges else {
            return "Reading only. Your Mac's Latch doesn't offer changes yet."
        }
        if let reason = repository.mutationBlockReason { return reason }
        if repository.operationState != nil, repository.offersContinue,
           let reason = repository.continueBlockReason {
            return reason
        }
        return "Changes run on your Mac."
    }

    static let deniedDetail = """
    Latch needs Face ID, Touch ID, or your passcode before it shows a repository on your Mac. \
    Try again when you're ready.
    """
}

// MARK: - Header

/// Path, branch, and how this branch stands against what was last fetched.
struct RepositoryHeaderCard: View {
    struct FetchControl {
        let enabled: Bool
        let working: Bool
        let label: String
        let hint: String
        let action: () -> Void
    }

    let status: GitStatus
    /// Nil when this Mac doesn't fetch for the phone; the button is left out.
    var fetch: FetchControl?
    /// Opens the push review from the "↑N to push" pill, when this Mac pushes.
    var reviewPush: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(status.root)
                .font(.footnote.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .truncationMode(.head)
                .textSelection(.enabled)
                .accessibilityLabel("Path, \(status.root)")
            HStack(spacing: 8) {
                Label(status.branchLabel, systemImage: status.head.detached ? "smallcircle.filled.circle" : "arrow.triangle.branch")
                    .font(.headline)
                    .lineLimit(1)
                    .accessibilityLabel(status.head.detached ? "Detached HEAD at \(status.branchLabel)" : "Branch \(status.branchLabel)")
                Spacer(minLength: 4)
            }
            if let upstream = status.upstream, let ahead = upstream.ahead, let behind = upstream.behind,
               ahead > 0 || behind > 0 {
                // Side by side when they fit, stacked at large text sizes.
                let pills = ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { aheadBehindPills(ahead: ahead, behind: behind) }
                    VStack(alignment: .leading, spacing: 8) { aheadBehindPills(ahead: ahead, behind: behind) }
                }
                if let reviewPush, ahead > 0 {
                    Button(action: reviewPush) { pills }
                        .buttonStyle(.plain)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(GitPresentation.aheadBehindSpoken(status.upstream))
                        .accessibilityHint("Opens the push review")
                        .accessibilityAddTraits(.isButton)
                        .accessibilityIdentifier("repository.reviewPush")
                } else {
                    pills
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(GitPresentation.aheadBehindSpoken(status.upstream))
                }
            }
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    Text(GitPresentation.trackingLine(status, now: context.date))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                // Fetch is the only way the phone updates the counts; it is
                // never done on its own.
                if let fetch {
                    Button(action: fetch.action) {
                        HStack(spacing: 4) {
                            if fetch.working { ProgressView().controlSize(.mini) }
                            Text("Fetch")
                        }
                    }
                    .font(.footnote.weight(.semibold))
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .opacity(fetch.enabled ? 1 : 0.45)
                    .accessibilityLabel(fetch.label)
                    .accessibilityHint(fetch.hint)
                    .accessibilityIdentifier("repository.fetch")
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("repository.header")
    }

    @ViewBuilder
    private func aheadBehindPills(ahead: Int, behind: Int) -> some View {
        if ahead > 0 { GitOutlinedPill(text: "\u{2191}\(ahead) to push") }
        if behind > 0 { GitOutlinedPill(text: "\u{2193}\(behind) incoming") }
    }
}

// MARK: - Sessions working here

/// Who else is in this worktree. Information, not a lock.
struct SessionsWorkingHereCard: View {
    let sessions: [GitSessionRef]
    let open: ((String) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(sessions.workingHereTitle, systemImage: "person.2")
                .font(.subheadline.weight(.semibold))
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(sessions) { session in
                        Button {
                            open?(session.id)
                        } label: {
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(session.active ? Color.green : Color.gray)
                                    .frame(width: 8, height: 8)
                                Text(session.chipLabel)
                                    .lineLimit(1)
                            }
                            .font(.footnote)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(Color(.systemBackground).opacity(0.7), in: Capsule())
                        }
                        .buttonStyle(.plain)
                        .disabled(open == nil)
                        .accessibilityLabel(session.accessibilityLabel)
                        .accessibilityHint("Opens this session")
                    }
                }
            }
            Text("Files may change while you look.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.orange.opacity(0.14), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("repository.sessions")
    }
}

// MARK: - Rows

/// One changed file. Tapping the row opens its diff; the staging circle is
/// its own 44pt control, and swipe and long-press do the same as it.
struct GitChangeRow: View {
    struct Staging {
        let enabled: Bool
        let toggle: () -> Void
    }

    let entry: GitStatusEntry
    /// Nil when this file cannot be staged from the phone (a conflict, or a
    /// Mac that does not offer changes).
    var staging: Staging?
    let openDiff: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: openDiff) {
                HStack(spacing: 12) {
                    GitStatusBadge(entry: entry)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.fileName)
                            .font(.body)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if !entry.detail.isEmpty {
                            Text(entry.detail)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.head)
                        }
                    }
                    Spacer(minLength: 8)
                    if let counts = entry.lineCounts {
                        Text(counts)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel([entry.fileName, entry.detail].filter { !$0.isEmpty }.joined(separator: ", "))
            .accessibilityValue(entry.accessibilityStatus)
            .accessibilityHint("Opens the diff")
            .accessibilityIdentifier("repository.row")

            if let staging {
                Button(action: staging.toggle) {
                    Image(systemName: entry.area == .staged ? "checkmark.circle.fill" : "circle")
                        .font(.title3)
                        .foregroundStyle(entry.area == .staged ? Color.accentColor : .secondary)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .opacity(staging.enabled ? 1 : 0.35)
                .accessibilityLabel(entry.stagingLabel)
                .accessibilityHint(staging.enabled ? "" : "Not available right now")
                .accessibilityIdentifier("repository.stage")
            }
        }
        .listRowSeparator(.hidden)
        .listRowBackground(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.clear)
                .padding(.horizontal, 8)
        )
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            if let staging {
                Button(action: staging.toggle) {
                    Label(entry.area == .staged ? "Unstage" : "Stage",
                          systemImage: entry.area == .staged ? "minus.circle" : "plus.circle")
                }
                .tint(entry.area == .staged ? .orange : .accentColor)
            }
        }
        .contextMenu {
            Label(entry.path, systemImage: "doc")
            if let staging {
                Button(action: staging.toggle) {
                    Label(entry.area == .staged ? "Unstage" : "Stage",
                          systemImage: entry.area == .staged ? "minus.circle" : "plus.circle")
                }
            }
            Button(action: openDiff) {
                Label("Open diff", systemImage: "plus.forwardslash.minus")
            }
            Button {
                UIPasteboard.general.string = entry.path
            } label: {
                Label("Copy path", systemImage: "doc.on.doc")
            }
        }
    }
}

// MARK: - Details

struct RepositoryDetailsSheet: View {
    let status: GitStatus
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    stacked("Path", status.root, monospaced: true)
                    LabeledContent("Kind", value: kind)
                    LabeledContent("Branch", value: status.head.branch ?? "Detached HEAD")
                    if let oid = status.head.oid {
                        stacked("Commit", oid, monospaced: true)
                    }
                    LabeledContent("Upstream", value: status.upstream?.name ?? "None")
                    LabeledContent("Last fetched", value: GitPresentation.fetchedPhrase(status.lastFetchedAt))
                }
                Section {
                    Text("Latch shows exactly what Git reports on your Mac. Opening or refreshing never fetches, so the counts are as of the last fetch.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle(status.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private var kind: String {
        switch status.kind {
        case .repository: "Repository"
        case .linkedWorktree: "Linked worktree"
        case .submodule: "Submodule"
        }
    }

    private func stacked(_ label: String, _ value: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
            Text(value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// A list row with no separator, inset, or background of its own.
    func plainRow() -> some View {
        listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
            .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
    }
}
