import LatchMobileKit
import SwiftUI

// MARK: - Review push

/// The push review, pushed inside the Sync sheet: where the commits go,
/// every commit that goes, and one button. A branch without an upstream
/// first asks where to push; the Mac's suggestion only fills the fields.
/// Force push, reset, clean and branch deletion are never offered here.
struct PushReviewView: View {
    let repository: GitRepositoryModel
    @Bindable var model: GitPushModel
    /// Back to the Sync sheet's root, fetching `remote` there.
    let backToSync: (_ remote: String?) -> Void
    /// Closes the whole Sync sheet after a push went through.
    let closeSheet: () -> Void

    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var blockedReason: String?
    @FocusState private var destinationFocused: Bool

    private var stale: Bool { repository.isStale }
    private var runner: GitMutationRunner { repository.mutations }

    var body: some View {
        List {
            if let progress = repository.progress, model.sentRequestIDs.contains(progress.requestId) {
                GitProgressCard(
                    progress: progress,
                    stopWaiting: {
                        repository.stopWaiting()
                        closeSheet()
                    },
                    checkRemote: repository.offersRemoteCheck ? { Task { await model.checkRemote() } } : nil
                )
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
            } else if let parked = runner.unresolvedPush {
                unresolvedCard(parked.action)
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
                    if model.needsSelection {
                        selectionSection(preview)
                    } else {
                        refsSection(preview)
                        notesSection(preview)
                        outgoingSection(preview)
                    }
                }
                if let notice = model.notice {
                    Text(notice).font(.footnote).foregroundStyle(.secondary).plainRow()
                }
                if let error = model.error, model.preview != nil {
                    Text(error).font(.footnote).foregroundStyle(.secondary).plainRow()
                }
            }
            .opacity(stale ? 0.55 : 1)
            if dynamicTypeSize.isAccessibilitySize, let preview = model.preview, !model.needsSelection {
                actionBar(preview).plainRow()
            }
        }
        .overlay {
            if model.preview == nil {
                if model.isLoading {
                    ProgressView("Reading the push review…")
                } else if let error = model.error {
                    MessageView(icon: "exclamationmark.triangle", title: "Cannot read the push review", detail: error)
                }
            }
        }
        .navigationTitle("Review push")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !dynamicTypeSize.isAccessibilitySize, let preview = model.preview, !model.needsSelection { actionBar(preview) }
        }
        .refreshable { await model.load() }
        .task { if model.preview == nil { await model.load() } }
        .onChange(of: runner.phase) { _, phase in
            // Pushed: the repository shows the toast and the new state.
            guard case .completed(let action, _) = phase, action.kind == .push,
                  model.sentRequestIDs.contains(action.requestId) || runner.remoteCheck?.receipt.requestId == action.requestId
            else { return }
            closeSheet()
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

    /// The outcome of any push: one sent from here, or one an earlier
    /// review left uncertain.
    private var outcome: GitOutcomePresentation? {
        guard let action = runner.phase.action, action.kind == .push,
              let presentation = repository.outcomePresentation,
              presentation.style != .completed
        else { return nil }
        return presentation
    }

    private func act(_ step: GitOutcomePresentation.NextStep) async {
        switch step {
        case .checkRemote:
            await model.checkRemote()
        case .pushAgain:
            await model.pushAgain()
        case .reviewPushAgain:
            await model.reviewAgain()
        case .fetchAndChooseUpdate:
            let remote: String? = if case .push(let request)? = runner.phase.action { request.remote } else { nil }
            await repository.acknowledgeOutcome()
            backToSync(remote)
        case .dismiss:
            await repository.acknowledgeOutcome()
        default:
            await repository.perform(step)
            await model.load()
        }
    }

    /// A push whose outcome was put aside: it must be settled first.
    private func unresolvedCard(_ action: GitMutationAction) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Your last push is unconfirmed", systemImage: "questionmark.circle.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.orange)
            Text(runner.remoteCheckNote ?? "Latch couldn't tell whether it reached the remote. Check the remote before pushing again.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if repository.offersRemoteCheck {
                Button("Check the remote") { Task { await model.checkRemote() } }
                    .buttonStyle(.bordered)
                    .disabled(runner.phase.isWorking)
                    .accessibilityIdentifier("push.checkRemote")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
    }

    // MARK: Destination

    private func selectionSection(_ preview: GitPushPreview) -> some View {
        Section {
            if model.remoteChoices.count > 1 {
                Picker("Remote", selection: $model.draftRemote) {
                    if !model.remoteChoices.contains(model.draftRemote) {
                        Text("Choose…").tag(model.draftRemote)
                    }
                    ForEach(model.remoteChoices, id: \.self) { remote in
                        Text(remote).tag(remote)
                    }
                }
                .accessibilityIdentifier("push.remote")
            } else if let only = model.remoteChoices.first {
                LabeledContent("Remote", value: only)
            }
            TextField("Branch on the remote", text: $model.draftDestination)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.body.monospaced())
                .focused($destinationFocused)
                .submitLabel(.done)
                .accessibilityLabel("Branch on the remote")
                .accessibilityIdentifier("push.destination")
            Button {
                destinationFocused = false
                Task { await model.reviewSelection() }
            } label: {
                HStack {
                    if model.isLoading { ProgressView() }
                    Text("Review").font(.body.weight(.semibold))
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!model.canReviewSelection || stale)
            .accessibilityHint(model.selectionBlockReason ?? "Shows what would be pushed there")
            .accessibilityIdentifier("push.review")
        } header: {
            Text("Where to push \(preview.source?.branch ?? "this branch")")
        } footer: {
            VStack(alignment: .leading, spacing: 4) {
                if let reason = preview.selectionReason { Text(reason) }
                Text(model.selectionBlockReason ?? "Check the remote and branch name, then review. Nothing is pushed until you confirm on the next step.")
            }
        }
    }

    // MARK: Review

    private func refsSection(_ preview: GitPushPreview) -> some View {
        Section {
            if let refs = preview.refsLine {
                Text(refs)
                    .font(.callout.monospaced())
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel(preview.refsSpoken ?? refs)
                    .accessibilityIdentifier("push.refs")
            }
            if let target = preview.target {
                VStack(alignment: .leading, spacing: 6) {
                    Label(target.remote, systemImage: "network")
                        .font(.headline)
                    if let url = preview.remoteURL {
                        Text(url)
                            .font(.footnote.monospaced())
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                    TimelineView(.periodic(from: .now, by: 30)) { context in
                        Text("\(target.name) · \(preview.fetchedLine(now: context.date))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 2)
                .accessibilityElement(children: .combine)
            }
            if model.offersChangeDestination {
                Button("Change destination…") { Task { await model.changeDestination() } }
                    .font(.subheadline)
                    .disabled(runner.phase.blocksNewActions)
            }
        } header: {
            Text("Push")
        }
    }

    @ViewBuilder
    private func notesSection(_ preview: GitPushPreview) -> some View {
        // (text, symbol, is a warning). The default-branch and new-branch
        // notes are neutral; a likely rejection is orange, and Push stays on.
        let notes: [(String, String, Bool)] = [
            preview.defaultBranchNote.map { ($0, "info.circle", false) },
            preview.newBranchNote.map { ($0, "plus.circle", false) },
            preview.rejectionLikelyReason.map {
                ("\($0) The remote will probably refuse this push; fetch and update first.", "exclamationmark.triangle.fill", true)
            },
        ].compactMap { $0 }
        if !notes.isEmpty {
            Section {
                ForEach(Array(notes.enumerated()), id: \.offset) { _, note in
                    Label {
                        Text(note.0)
                            .foregroundStyle(note.2 ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                    } icon: {
                        Image(systemName: note.1)
                            .foregroundStyle(note.2 ? AnyShapeStyle(Color.orange) : AnyShapeStyle(.secondary))
                    }
                    .font(.footnote)
                    .fixedSize(horizontal: false, vertical: true)
                    .listRowBackground(note.2 ? Color.orange.opacity(0.14) : Color(.secondarySystemGroupedBackground))
                    .accessibilityIdentifier(note.2 ? "push.rejectionLikely" : "push.note")
                }
            }
        }
    }

    private func outgoingSection(_ preview: GitPushPreview) -> some View {
        Section {
            if model.outgoing.isEmpty {
                Text(preview.target?.newBranch == true && preview.ahead == nil
                     ? "Latch can't list the commits a new branch sends until it has been fetched."
                     : "No commits to push as last fetched.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(model.outgoing) { commit in
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
                .accessibilityIdentifier("push.loadMore")
            }
        } header: {
            Text(preview.outgoingHeader)
                .accessibilityLabel(preview.ahead.map { "Outgoing, \(GitPushPresentation.commits($0))" } ?? "Outgoing")
        }
    }

    // MARK: Action bar

    private func actionBar(_ preview: GitPushPreview) -> some View {
        VStack(spacing: 8) {
            if let warning = GitCommitMessage.activeSessionsWarning(repository.status?.sessions ?? []) {
                GitActiveSessionsNote(text: warning)
            }
            if let reason = model.pushBlockReason {
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button {
                guard model.canPush else {
                    blockedReason = model.pushBlockReason
                    return
                }
                Task { await model.push() }
            } label: {
                HStack {
                    if runner.phase.isWorking, runner.phase.action?.kind == .push { ProgressView() }
                    Text(preview.pushButtonTitle)
                        .font(.body.weight(.semibold))
                        .lineLimit(dynamicTypeSize.isAccessibilitySize ? nil : 2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!model.canPush)
            .accessibilityLabel(preview.pushSpokenLabel)
            .accessibilityHint(model.pushBlockReason ?? GitPushPresentation.caption)
            .accessibilityIdentifier("push.push")
            Text(GitPushPresentation.caption)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
        }
        .padding(16)
        .background(.bar)
    }
}
