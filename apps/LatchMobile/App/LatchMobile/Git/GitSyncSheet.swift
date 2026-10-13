import LatchMobileKit
import SwiftUI

// MARK: - Sync

/// The remote, the incoming commits, and the three ways to bring them in.
/// One decision that ends with one button, like the other Git sheets.
/// Reading it never fetches; Fetch is the remote card's button.
struct SyncSheet: View {
    let repository: GitRepositoryModel
    let model: GitSyncModel

    /// Screens pushed inside the sheet.
    enum Route: Hashable {
        case push
    }

    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var remoteChoices: [String] = []
    @State private var blockedReason: String?
    @State private var fetchedMessage: String?
    @State private var path: [Route]
    @State private var pushModel: GitPushModel?

    /// `push`, when given, opens the sheet on the push review.
    init(repository: GitRepositoryModel, model: GitSyncModel, push: GitPushModel? = nil) {
        self.repository = repository
        self.model = model
        _path = State(initialValue: push == nil ? [] : [.push])
        _pushModel = State(initialValue: push)
    }

    /// Requests sent from this sheet, so only their outcomes show here.
    private var sentHere: Set<String> { model.sentRequestIDs }

    private var stale: Bool { repository.isStale }

    var body: some View {
        NavigationStack(path: $path) {
            List {
                if let progress = repository.progress, sentHere.contains(progress.requestId) {
                    GitProgressCard(progress: progress) {
                        repository.stopWaiting()
                        dismiss()
                    }
                    .plainRow()
                } else if let presentation = outcome {
                    GitOutcomeCard(
                        presentation: presentation,
                        primary: { Task { await act(presentation.nextStep) } },
                        dismiss: { Task { await repository.acknowledgeOutcome() } },
                        secondary: presentation.secondaryStep.map { step in
                            { Task { await act(step) } }
                        }
                    )
                    .plainRow()
                }
                if stale {
                    TimelineView(.periodic(from: .now, by: 30)) { context in
                        GitStalePill(text: repository.stalePill(now: context.date))
                    }
                    .frame(maxWidth: .infinity)
                    .plainRow()
                }
                Group {
                    if let preview = model.preview {
                        remoteSection(preview)
                        incomingSection(preview)
                        if preview.outgoingLine != nil || offersPushWithoutUpstream(preview) {
                            outgoingSection(preview)
                        }
                        if !model.availableModes.isEmpty {
                            modeSection
                        }
                    }
                    if let notice = model.notice {
                        Text(notice).font(.footnote).foregroundStyle(.secondary).plainRow()
                    }
                    if let error = model.error {
                        Text(error).font(.footnote).foregroundStyle(.secondary).plainRow()
                    }
                }
                .opacity(stale ? 0.55 : 1)
                if dynamicTypeSize.isAccessibilitySize, model.preview != nil, !model.availableModes.isEmpty {
                    actionBar.plainRow()
                }
            }
            .overlay {
                if model.preview == nil {
                    if model.isLoading {
                        ProgressView("Reading the upstream…")
                    } else if let error = model.error {
                        MessageView(icon: "exclamationmark.triangle", title: "Cannot read the upstream", detail: error)
                    }
                }
            }
            .navigationTitle("Sync")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .push:
                    if let pushModel {
                        PushReviewView(
                            repository: repository, model: pushModel,
                            backToSync: { remote in returnFromPush(fetching: remote) },
                            closeSheet: { dismiss() }
                        )
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) {
                                Button("Close") { dismiss() }
                            }
                        }
                    }
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if !dynamicTypeSize.isAccessibilitySize, model.preview != nil, !model.availableModes.isEmpty { actionBar }
            }
            .refreshable { await model.load() }
            .task { if model.preview == nil { await model.load() } }
            .onChange(of: repository.mutations.phase) { _, phase in
                guard case .completed(let action, let receipt) = phase, sentHere.contains(action.requestId) else { return }
                switch action.kind {
                case .fetch:
                    fetchedMessage = receipt.outcome?.message ?? "Fetched."
                case .fastForward, .merge, .rebase:
                    // Done: the repository shows the toast and the new state.
                    // A stopped merge or rebase stays to say what happened.
                    if receipt.outcome?.code != "stopped" { dismiss() }
                default:
                    break
                }
            }
            .confirmationDialog(
                "Fetch from which remote?",
                isPresented: Binding(get: { !remoteChoices.isEmpty }, set: { if !$0 { remoteChoices = [] } }),
                titleVisibility: .visible
            ) {
                ForEach(remoteChoices, id: \.self) { remote in
                    Button(remote) { fetch(remote) }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This branch doesn't track a remote branch, so choose which remote to fetch.")
            }
            .alert(
                "This phone can't do that right now",
                isPresented: Binding(get: { blockedReason != nil }, set: { if !$0 { blockedReason = nil } })
            ) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(blockedReason ?? "")
            }
        }
    }

    /// Only the outcome of a change sent from this sheet.
    private var outcome: GitOutcomePresentation? {
        guard let action = repository.mutations.phase.action, sentHere.contains(action.requestId),
              let presentation = repository.outcomePresentation,
              presentation.style != .completed
        else { return nil }
        return presentation
    }

    private func act(_ step: GitOutcomePresentation.NextStep) async {
        switch step {
        case .chooseMergeOrRebase:
            await repository.acknowledgeOutcome()
            model.chooseMergeOrRebase()
            await model.load()
        case .dismiss:
            // After a stopped merge or rebase the repository's operation
            // card is where the person goes next.
            let stopped = outcome?.style == .stopped
            await repository.acknowledgeOutcome()
            if stopped { dismiss() }
        case .fetchAgain:
            await model.fetchAgain()
        default:
            await repository.perform(step)
            await model.load()
        }
    }

    // MARK: Remote

    private func remoteSection(_ preview: GitSyncPreview) -> some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline) {
                    Label(remoteName(preview) ?? "No remote", systemImage: "network")
                        .font(.headline)
                    Spacer(minLength: 8)
                    if model.support.fetch {
                        Button {
                            startFetch()
                        } label: {
                            HStack(spacing: 4) {
                                if isFetching { ProgressView().controlSize(.small) }
                                Text("Fetch")
                            }
                        }
                        .font(.footnote.weight(.semibold))
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .opacity(model.canFetch ? 1 : 0.45)
                        .accessibilityLabel(remoteName(preview).map { "Fetch \($0)" } ?? "Fetch")
                        .accessibilityHint(model.fetchBlockReason ?? "Updates what Latch knows about the remote")
                        .accessibilityIdentifier("sync.fetch")
                    }
                }
                if let url = preview.upstream?.remoteUrl ?? preview.remotes.first(where: { $0.name == remoteName(preview) })?.url {
                    Text(url)
                        .font(.footnote.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    Text(preview.upstream.map { "\($0.name) · \(preview.fetchedLine(now: context.date))" }
                         ?? preview.fetchedLine(now: context.date).capitalizedFirst)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let fetchedMessage {
                    Label(fetchedMessage, systemImage: "checkmark.circle.fill")
                        .font(.footnote)
                        .foregroundStyle(.green)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.vertical, 4)
            .accessibilityElement(children: .contain)
        } header: {
            Text("Remote")
        }
    }

    private func remoteName(_ preview: GitSyncPreview) -> String? {
        preview.upstream?.remote ?? (preview.remotes.count == 1 ? preview.remotes.first?.name : nil)
    }

    private var isFetching: Bool {
        guard let action = repository.mutations.phase.action, action.kind == .fetch else { return false }
        return repository.mutations.phase.isWorking
    }

    private func startFetch() {
        guard model.canFetch else {
            blockedReason = model.fetchBlockReason
            return
        }
        let remotes = model.fetchRemotes
        if remotes.count == 1 {
            fetch(remotes[0])
        } else {
            remoteChoices = remotes
        }
    }

    private func fetch(_ remote: String) {
        fetchedMessage = nil
        Task { await model.fetch(remote: remote) }
    }

    // MARK: Incoming

    private func incomingSection(_ preview: GitSyncPreview) -> some View {
        Section {
            if model.incoming.isEmpty {
                Text(preview.relationExplanation ?? "No incoming commits as last fetched.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(model.incoming) { commit in
                VStack(alignment: .leading, spacing: 2) {
                    Text(commit.subject)
                        .font(.subheadline)
                        .lineLimit(2)
                    HStack(spacing: 6) {
                        Text(commit.shortOid).font(.caption.monospaced())
                        Text(commit.byline()).font(.caption)
                    }
                    .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
            if model.hasMore {
                Button {
                    Task { await model.loadMore() }
                } label: {
                    HStack {
                        Text("Load more")
                        if model.isLoading { ProgressView().controlSize(.small) }
                    }
                }
                .disabled(model.isLoading || stale)
                .accessibilityIdentifier("sync.loadMore")
            }
        } header: {
            Text(incomingHeader(preview))
        }
    }

    private func incomingHeader(_ preview: GitSyncPreview) -> String {
        let count = preview.incomingCount
        guard count > 0 else { return "Incoming" }
        return "Incoming \u{2193}\(count)"
    }

    /// A branch with commits but no upstream can still be pushed, once the
    /// person chooses where.
    private func offersPushWithoutUpstream(_ preview: GitSyncPreview) -> Bool {
        repository.offersPush && preview.relation == .noUpstream && preview.head.oid != nil
    }

    private func outgoingSection(_ preview: GitSyncPreview) -> some View {
        Section {
            HStack {
                if let outgoing = preview.outgoingLine {
                    Label(outgoing, systemImage: "arrow.up")
                        .accessibilityLabel(GitPresentation.aheadBehindSpoken(
                            GitUpstream(name: preview.upstream?.name ?? "upstream", ahead: preview.ahead, behind: 0)
                        ))
                } else {
                    Label("No upstream yet", systemImage: "arrow.up")
                }
                Spacer()
                // Older Macs don't push for the phone; the entry point is
                // left out rather than shown unavailable.
                if repository.offersPush {
                    let enabled = repository.pushBlockReason == nil
                    Button {
                        openPush()
                    } label: {
                        Text("Review push…")
                    }
                    .opacity(enabled ? 1 : 0.45)
                    .accessibilityHint(repository.pushBlockReason ?? "Shows the remote, the destination and every commit before pushing")
                    .accessibilityIdentifier("sync.reviewPush")
                }
            }
            .font(.subheadline)
        } header: {
            Text("Outgoing")
        }
    }

    private func openPush() {
        guard repository.pushBlockReason == nil else {
            blockedReason = repository.pushBlockReason
            return
        }
        guard let push = repository.pushModel() else { return }
        pushModel = push
        path = [.push]
    }

    /// "Fetch and choose how to update": back to Sync, fetching the remote
    /// the push went to, so the incoming commits and the ways to bring them
    /// in are current.
    private func returnFromPush(fetching remote: String?) {
        path = []
        Task {
            await model.load()
            if let remote, model.canFetch, model.fetchRemotes.contains(remote) {
                fetchedMessage = nil
                await model.fetch(remote: remote)
            }
        }
    }

    // MARK: Mode

    private var modeSection: some View {
        Section {
            ForEach(model.availableModes) { mode in
                let enabled = model.isModeEnabled(mode)
                Button {
                    guard enabled else {
                        blockedReason = model.modeBlockedReason(mode)
                        return
                    }
                    model.mode = mode
                } label: {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: model.mode == mode ? "largecircle.fill.circle" : "circle")
                            .foregroundStyle(model.mode == mode ? Color.accentColor : .secondary)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(mode.title).font(.body.weight(.medium))
                            Text(model.modeBlockedReason(mode) ?? mode.explanation)
                                .font(.caption)
                                .foregroundStyle(model.modeBlockedReason(mode) != nil ? .orange : .secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .opacity(enabled ? 1 : 0.5)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(model.mode == mode ? .isSelected : [])
                .accessibilityHint(enabled ? "" : "Not available")
                .accessibilityIdentifier("sync.mode.\(mode.rawValue)")
            }
        } header: {
            Text("Bring them in by")
        }
    }

    // MARK: Action bar

    private var actionBar: some View {
        VStack(spacing: 8) {
            if let warning = GitCommitMessage.activeSessionsWarning(repository.status?.sessions ?? []) {
                GitActiveSessionsNote(text: warning)
            }
            if let refs = model.preview?.refsLine {
                Text(refs)
                    .font(.callout.monospaced())
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityLabel(refs.replacingOccurrences(of: "\u{2192}", with: "into"))
            }
            if let reason = model.updateBlockReason {
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button {
                guard model.canUpdate else {
                    blockedReason = model.updateBlockReason
                    return
                }
                Task { await model.update() }
            } label: {
                HStack {
                    if repository.mutations.phase.isWorking { ProgressView() }
                    Text(model.primaryTitle)
                        .font(.body.weight(.semibold))
                        .lineLimit(dynamicTypeSize.isAccessibilitySize ? nil : 2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!model.canUpdate)
            .accessibilityIdentifier("sync.update")
            Text("If Git stops on conflicts, resolve them on your Mac, then continue here.")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
        }
        .padding(16)
        .background(.bar)
    }
}

private extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

// MARK: - Progress

/// Git's progress while a long change runs on the Mac. "Stop waiting" only
/// puts this away: Git keeps running, and the receipt is still read.
struct GitProgressCard: View {
    let progress: GitProgressPresentation
    let stopWaiting: () -> Void
    /// "Check the remote" for a push that is no longer being read.
    var checkRemote: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                ProgressView()
                Text(progress.title)
                    .font(.headline)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
            }
            Text(progress.message)
                .font(.subheadline.monospaced())
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.updatesFrequently)
            if let fraction = progress.fraction {
                ProgressView(value: fraction)
                    .accessibilityLabel("Progress")
                    .accessibilityValue("\(Int(fraction * 100)) percent")
            }
            TimelineView(.periodic(from: .now, by: 1)) { context in
                if let elapsed = progress.elapsed(now: context.date) {
                    Text(elapsed)
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            Text(progress.stopWaitingNote)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let checkRemote {
                Button("Check the remote", action: checkRemote)
                    .buttonStyle(.bordered)
                    .frame(maxWidth: .infinity)
                    .accessibilityHint("Asks your Mac to look at the remote for this push.")
                    .accessibilityIdentifier("git.checkRemote")
            }
            Button("Stop waiting", action: stopWaiting)
                .buttonStyle(.bordered)
                .frame(maxWidth: .infinity)
                .accessibilityHint("Hides this view. Git keeps running on your Mac.")
                .accessibilityIdentifier("git.stopWaiting")
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("git.progress")
    }
}

// MARK: - Merge or rebase in progress

/// The orange card naming a merge or rebase that stopped part-way: its
/// refs, where it stopped, how many files conflict, and what to do.
struct OperationCard: View {
    let state: GitOperationState
    let branch: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(state.title, systemImage: "exclamationmark.triangle.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.orange)
            if let refs = state.refs(branch: branch) {
                Text(refs)
                    .font(.footnote.monospaced())
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
            if let step = state.stepLine {
                Text(step).font(.footnote)
            }
            if let conflicts = state.conflictLine {
                Label(conflicts, systemImage: "exclamationmark.circle")
                    .font(.footnote.weight(.medium))
            }
            Text(state.controlled == nil
                 ? "Finish it on your Mac or in a terminal session."
                 : GitOperationState.guidance)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.orange.opacity(0.14), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("repository.operation")
    }
}
