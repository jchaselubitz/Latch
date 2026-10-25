import LatchMobileKit
import SwiftUI

// MARK: - Outcomes

/// What became of the last change, in the design's four presentations:
/// a toast for completed, and a result card for rejected, refresh required
/// and uncertain. The card is shown in a sheet over the repository and
/// inline at the top of the Commit and Branches sheets.
struct GitOutcomeCard: View {
    let presentation: GitOutcomePresentation
    let primary: () -> Void
    var dismiss: (() -> Void)?
    /// The outcome's second button ("Fetch again"), when it has one.
    var secondary: (() -> Void)?
    @State private var showingGitOutput = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: icon)
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(tint)
                    .accessibilityHidden(true)
                Text(presentation.headline)
                    .font(.headline)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
            }
            if !presentation.detail.isEmpty {
                Text(presentation.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !presentation.files.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(presentation.files, id: \.self) { file in
                        Text(file)
                            .font(.footnote.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Files Git named: \(presentation.files.joined(separator: ", "))")
            }
            if !presentation.timeline.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(presentation.timeline.enumerated()), id: \.offset) { _, step in
                        Label(step, systemImage: "circle.fill")
                            .labelStyle(TimelineLabelStyle())
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            if let output = presentation.gitOutput, !output.isEmpty {
                DisclosureGroup("Show Git's message", isExpanded: $showingGitOutput) {
                    ScrollView(.horizontal) {
                        Text(output)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                            .padding(.vertical, 6)
                    }
                    .dynamicTypeSize(...DynamicTypeSize.xxxLarge)
                }
                .font(.footnote)
            }
            HStack(spacing: 12) {
                Button(action: primary) {
                    HStack {
                        if presentation.isChecking { ProgressView() }
                        Text(presentation.primaryTitle).font(.body.weight(.semibold))
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(presentation.isChecking)
                if let secondary, let title = presentation.secondaryTitle {
                    Button(title, action: secondary)
                        .buttonStyle(.bordered)
                        .disabled(presentation.isChecking)
                }
                if let dismiss, presentation.style != .uncertain, presentation.style != .stopped {
                    Button("Close", action: dismiss)
                        .buttonStyle(.bordered)
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.10), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("git.outcome")
    }

    private var icon: String {
        switch presentation.style {
        case .completed: "checkmark.circle.fill"
        case .rejected, .refreshRequired: "xmark.circle.fill"
        case .uncertain: "questionmark.circle.fill"
        case .stopped: "exclamationmark.triangle.fill"
        }
    }

    private var tint: Color {
        switch presentation.style {
        case .completed: .green
        case .rejected, .refreshRequired: .red
        case .uncertain, .stopped: .orange
        }
    }
}

private struct TimelineLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            configuration.icon.font(.system(size: 6)).accessibilityHidden(true)
            configuration.title
        }
    }
}

/// The short glass note a completed change leaves above the bottom controls.
struct GitToast: View {
    let text: String

    var body: some View {
        FloatingControl(shape: .capsule, size: 40) {
            Label(text, systemImage: "checkmark.circle.fill")
                .font(.footnote.weight(.semibold))
                .lineLimit(2)
                .padding(.horizontal, 4)
        }
        .foregroundStyle(.primary)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.updatesFrequently)
        .accessibilityIdentifier("git.toast")
    }
}

/// The orange note that sessions in this worktree are active. It informs;
/// the Mac's precondition check is what stops a change acting on a state
/// nobody reviewed.
struct GitActiveSessionsNote: View {
    let text: String

    var body: some View {
        Label(text, systemImage: "person.2")
            .font(.footnote)
            .fixedSize(horizontal: false, vertical: true)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.orange.opacity(0.14), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .accessibilityIdentifier("git.activeSessions")
    }
}

extension GitRepositoryModel {
    /// Does what an outcome card's button says.
    func perform(
        _ step: GitOutcomePresentation.NextStep, openCommit: (() -> Void)? = nil, openSync: (() -> Void)? = nil,
        openPush: (() -> Void)? = nil
    ) async {
        switch step {
        case .dismiss:
            await acknowledgeOutcome()
        case .refresh:
            await acknowledgeOutcome()
            await refresh()
        case .checkReceipt:
            await mutations.reconcile()
        case .commitChanges:
            await acknowledgeOutcome()
            openCommit?()
        case .chooseMergeOrRebase:
            await acknowledgeOutcome()
            openSync?()
        case .fetchAgain:
            await fetchAgain()
        case .checkRemote:
            await checkRemote()
        case .pushAgain, .reviewPushAgain:
            // Both go back to the review: nothing is pushed until the person
            // has seen it again and tapped Push.
            await acknowledgeOutcome()
            openPush?()
        case .fetchAndChooseUpdate:
            await acknowledgeOutcome()
            openSync?()
        }
    }
}

// MARK: - Commit

/// Review the staged files, write the message, commit on the Mac.
struct CommitSheet: View {
    let repository: GitRepositoryModel

    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var draft: GitCommitDraftModel
    @AppStorage("git.commitDraft.googleDisclosureAccepted") private var disclosureAccepted = false
    @State private var showingDisclosure = false
    @State private var sent = false
    @FocusState private var summaryFocused: Bool

    init(repository: GitRepositoryModel) {
        self.repository = repository
        _draft = State(initialValue: repository.commitDraftModel())
    }

    private var summary: String { draft.summary }
    private var details: String { draft.details }
    private var status: GitStatus? { repository.status }
    private var staged: [GitStatusEntry] { repository.stagedEntries }
    private var trimmedSummary: String { GitCommitMessage.summary(summary) }

    var body: some View {
        NavigationStack {
            List {
                if let presentation = outcome {
                    GitOutcomeCard(presentation: presentation, primary: {
                        Task { await repository.perform(presentation.nextStep) }
                    }, dismiss: {
                        Task { await repository.acknowledgeOutcome() }
                    })
                    .plainRow()
                }
                Section {
                    TextField("Summary", text: Binding(get: { draft.summary }, set: { draft.editSummary($0) }), axis: .vertical)
                        .lineLimit(1...3)
                        .focused($summaryFocused)
                        .submitLabel(.next)
                        .accessibilityIdentifier("commit.summary")
                        .onChange(of: summary) { _, value in
                            // One line: Return moves on instead of breaking it.
                            if value.contains("\n") {
                                draft.editSummary(value.replacingOccurrences(of: "\n", with: ""))
                                summaryFocused = false
                            }
                        }
                    TextField("Description (optional)", text: Binding(get: { draft.details }, set: { draft.editDetails($0) }), axis: .vertical)
                        .lineLimit(3...8)
                        .accessibilityIdentifier("commit.description")
                } header: {
                    Text("Message")
                } footer: {
                    let count = trimmedSummary.count
                    Text(count > GitCommitMessage.summaryGuide
                         ? "\(count) characters. Summaries read best under \(GitCommitMessage.summaryGuide)."
                         : "\(count) of \(GitCommitMessage.summaryGuide)")
                        .foregroundStyle(count > GitCommitMessage.summaryGuide ? .orange : .secondary)
                }
                draftSection
                Section {
                    ForEach(staged) { entry in
                        HStack(spacing: 10) {
                            GitStatusBadge(entry: entry)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(entry.fileName).lineLimit(1).truncationMode(.middle)
                                if !entry.detail.isEmpty {
                                    Text(entry.detail).font(.caption).foregroundStyle(.secondary)
                                        .lineLimit(1).truncationMode(.head)
                                }
                            }
                            Spacer(minLength: 6)
                            if let counts = entry.lineCounts {
                                Text(counts).font(.caption.monospaced()).foregroundStyle(.secondary)
                            }
                        }
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(entry.fileName)
                        .accessibilityValue(entry.accessibilityStatus)
                    }
                } header: {
                    Text("Staged \(staged.count)")
                } footer: {
                    if let note = GitCommitMessage.exclusionNote(repository.excludedFileCount) {
                        Text(note)
                    }
                }
                Section {
                    Label("The commit uses the Git name and email on your Mac, and any commit hooks there run as usual.", systemImage: "desktopcomputer")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                if dynamicTypeSize.isAccessibilitySize { actionBar.plainRow() }
            }
            .navigationTitle("Commit")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        draft.dismiss()
                        dismiss()
                    }
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if !dynamicTypeSize.isAccessibilitySize { actionBar }
            }
            .onAppear { summaryFocused = !dynamicTypeSize.isAccessibilitySize }
            .onDisappear { draft.dismiss() }
            .onChange(of: draft.identity) { _, _ in draft.sourceDidChange() }
            .onChange(of: ObjectIdentifier(repository)) { _, _ in
                draft.dismiss()
                draft = repository.commitDraftModel()
            }
            .alert("Send staged changes to Google?", isPresented: $showingDisclosure) {
                Button("Cancel", role: .cancel) {}
                Button("Send and Generate") {
                    disclosureAccepted = true
                    draft.startGeneration()
                }
            } message: {
                Text("Your Mac sends selected staged source changes to Google Gemini to draft a message. Context is filtered and bounded, so some changes may be omitted. The Gemini key stays on your Mac. Review the editable result before committing.")
            }
            .onChange(of: repository.mutations.phase) { _, phase in
                // Done: the repository shows the toast and the new state.
                if sent, case .completed(let action, _) = phase, action.kind == .commit {
                    dismiss()
                }
            }
        }
    }

    private var draftSection: some View {
        Section {
            if draft.isGenerating {
                HStack {
                    ProgressView().accessibilityLabel("Generating commit message")
                    Text("Generating from staged changes…")
                }
                Button("Cancel Generation", role: .cancel) { draft.cancelGeneration() }
                    .accessibilityIdentifier("commit.cancelGeneration")
            } else {
                Button {
                    if disclosureAccepted { draft.startGeneration() }
                    else { showingDisclosure = true }
                } label: {
                    Label(draft.result == nil ? "Generate Message" : "Regenerate Message", systemImage: "sparkles")
                }
                .disabled(!draft.canGenerate)
                .accessibilityHint("Sends selected staged source changes from your Mac to Google Gemini.")
                .accessibilityIdentifier("commit.generate")
            }
            if let note = draft.availabilityNote {
                Text(note).font(.footnote).foregroundStyle(.secondary)
            }
            if let notice = draft.notice {
                Text(notice).font(.footnote)
                    .accessibilityIdentifier("commit.draftNotice")
            }
            if let coverage = draft.coverageNote {
                Label(coverage, systemImage: "exclamationmark.triangle")
                    .font(.footnote).foregroundStyle(.orange)
            }
            if draft.requiresReview {
                Text("This draft describes an earlier branch, HEAD or staged index. Review all current staged files and the message, or regenerate before committing.")
                    .font(.footnote).foregroundStyle(.orange)
                Button("I Reviewed the Message and Staged Files") { draft.acknowledgeManualReview() }
                    .disabled(!repository.canCommit || draft.isGenerating)
                    .accessibilityIdentifier("commit.reviewDraft")
            }
        } header: {
            Text("AI Draft")
        } footer: {
            Text("Generate sends selected staged changes to Google. Review for accuracy; generation does not commit or push.")
        }
    }

    /// Only this sheet's own commit is shown here.
    private var outcome: GitOutcomePresentation? {
        guard sent, repository.mutations.phase.action?.kind == .commit,
              let presentation = GitOutcomePresentation.of(repository.mutations.phase),
              presentation.style != .completed
        else { return nil }
        return presentation
    }

    private var actionBar: some View {
        VStack(spacing: 8) {
            if let warning = GitCommitMessage.activeSessionsWarning(status?.sessions ?? []) {
                GitActiveSessionsNote(text: warning)
            }
            if let reason = repository.commitBlockReason {
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button {
                sent = true
                Task {
                    guard draft.canCommitMessage else { return }
                    await repository.commit(summary: summary, description: details)
                }
            } label: {
                HStack {
                    if repository.mutations.phase.isWorking { ProgressView() }
                    Text(GitCommitMessage.buttonTitle(fileCount: staged.count, branch: status?.head.branch))
                        .font(.body.weight(.semibold))
                        .lineLimit(dynamicTypeSize.isAccessibilitySize ? nil : 2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!repository.canCommit || trimmedSummary.isEmpty || !draft.canCommitMessage)
            .accessibilityIdentifier("commit.button")
            Text("Creates a commit on your Mac. Nothing is pushed.")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
        }
        .padding(16)
        .background(.bar)
    }
}

// MARK: - Branches

/// Local branches: the current one first, then search, a new branch from
/// the current one, and the rest. Git's refusal to switch shows on top.
struct BranchesSheet: View {
    let repository: GitRepositoryModel
    let model: GitBranchesModel
    let openCommit: () -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var creating = false
    @State private var newName = ""
    @State private var switchToNew = true
    @State private var blockedReason: String?

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            List {
                if let presentation = outcome {
                    GitOutcomeCard(presentation: presentation, primary: {
                        Task {
                            await repository.perform(presentation.nextStep) {
                                dismiss()
                                openCommit()
                            }
                            await model.load()
                        }
                    }, dismiss: {
                        Task { await repository.acknowledgeOutcome() }
                    })
                    .plainRow()
                }
                if let warning = GitCommitMessage.activeSessionsWarning(repository.status?.sessions ?? []) {
                    GitActiveSessionsNote(text: warning).plainRow()
                }
                if let current = model.current {
                    Section("Current") {
                        branchRow(current)
                    }
                } else if let head = model.list?.head, head.detached {
                    Section("Current") {
                        Label("Detached at \(String((head.oid ?? "").prefix(7)))", systemImage: "smallcircle.filled.circle")
                    }
                }
                if model.support.createBranch, let start = model.startPoint {
                    Section {
                        if creating {
                            TextField("Branch name", text: $newName)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .font(.body.monospaced())
                                .accessibilityIdentifier("branches.newName")
                            Toggle("Switch to it", isOn: $switchToNew)
                            Button {
                                guard model.canCreate else {
                                    blockedReason = repository.mutationBlockReason
                                    return
                                }
                                Task { await model.createBranch(named: newName, switchToIt: switchToNew) }
                            } label: {
                                Text("Create from \(start.name)")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(!GitBranchName.isPlausible(newName.trimmingCharacters(in: .whitespaces))
                                      || repository.mutations.phase.isWorking)
                        } else {
                            Button {
                                creating = true
                            } label: {
                                Label("New branch from \(start.name)…", systemImage: "plus")
                            }
                            .opacity(model.canCreate ? 1 : 0.5)
                        }
                    } footer: {
                        if creating {
                            Text("Starts at \(String(start.oid.prefix(7))). If switching would overwrite local changes, Git refuses and nothing is stashed or discarded.")
                        }
                    }
                }
                Section {
                    ForEach(model.others) { branch in
                        branchRow(branch)
                    }
                    if model.list?.truncated == true {
                        Text("More branches exist than fit here. Search narrows the list.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Text("Local branches")
                }
                if let error = model.error {
                    Text(error).font(.footnote).foregroundStyle(.secondary).plainRow()
                }
            }
            .searchable(text: $model.query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search branches")
            .overlay {
                if model.list == nil, model.isLoading {
                    ProgressView("Reading branches…")
                }
            }
            .navigationTitle("Branches")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .refreshable { await model.load() }
            .task { if model.list == nil { await model.load() } }
            .onChange(of: repository.mutations.phase) { _, phase in
                if case .completed(let action, _) = phase, action.kind == .createBranch {
                    creating = false
                    newName = ""
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
        }
    }

    private var outcome: GitOutcomePresentation? {
        guard let action = repository.mutations.phase.action,
              action.kind == .createBranch || action.kind == .switchBranch,
              let presentation = GitOutcomePresentation.of(repository.mutations.phase),
              presentation.style != .completed
        else { return nil }
        return presentation
    }

    private func branchRow(_ branch: GitBranch) -> some View {
        Button {
            guard !branch.current else { return }
            guard model.canSwitch(to: branch) else {
                blockedReason = branch.checkedOutElsewhere
                    ? "\(branch.name) is checked out in another worktree on your Mac. Git won't switch to it here."
                    : repository.mutationBlockReason ?? "Switching branches isn't available."
                return
            }
            Task { await model.switchBranch(to: branch) }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: branch.current ? "checkmark.circle.fill" : "arrow.triangle.branch")
                    .foregroundStyle(branch.current ? Color.accentColor : .secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(branch.name)
                        .font(.body.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let upstream = branch.upstreamLine {
                        Text(upstream).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    if !branch.subject.isEmpty {
                        Text(branch.subject).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 6)
                if branch.checkedOutElsewhere {
                    Text("Other worktree").font(.caption2).foregroundStyle(.secondary)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(branch.current || model.canSwitch(to: branch) ? 1 : 0.55)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(branch.current ? "\(branch.name), current branch" : branch.name)
        .accessibilityHint(branch.current ? "" : "Switches to this branch")
    }
}
