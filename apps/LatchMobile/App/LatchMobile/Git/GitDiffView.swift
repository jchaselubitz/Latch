import LatchMobileKit
import SwiftUI

/// One changed file's diff: unified hunks in monospace, with line numbers and
/// a +/− column so additions and deletions never rest on colour alone.
///
/// It reads a page at a time, within the Mac's limits, and says so when more
/// remains. Binary, oversized and submodule files explain themselves instead.
struct GitDiffView: View {
    let repository: GitRepositoryModel

    @Environment(AppModel.self) private var appModel
    @State private var entry: GitStatusEntry
    @State private var diff: GitDiffModel?
    @State private var blockedReason: String?

    init(repository: GitRepositoryModel, entry: GitStatusEntry) {
        self.repository = repository
        _entry = State(initialValue: entry)
    }

    var body: some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) {
                SessionChromeBar(
                    title: entry.fileName,
                    status: GitPresentation.diffStatus(area: entry.area, diff: diff?.diff),
                    statusIdentifier: "diff.chrome.status",
                    showDetails: {}
                )
            }
            .safeAreaInset(edge: .bottom, spacing: 0) { bottomBar }
            .toolbar(.hidden, for: .navigationBar)
            .background(InteractivePopRestorer())
            .task(id: entry.id) {
                let model = repository.diffModel(for: entry)
                diff = model
                await model.load()
            }
            // After staging or unstaging, the same file is now listed in its
            // other area; follow it there rather than show a stale area.
            .onChange(of: repository.status) { _, status in
                guard let entries = status?.entries, !entries.contains(where: { $0.id == entry.id }),
                      let moved = entries.first(where: { $0.pathKey == entry.pathKey })
                else { return }
                entry = moved
            }
            .overlay(alignment: .top) {
                if repository.conflictMarkersQuestion == nil,
                   let presentation = GitOutcomePresentation.of(
                    repository.mutations.phase, lastReceipt: repository.mutations.lastReceipt,
                    connectionLostAt: repository.mutations.connectionLostAt
                   ),
                   presentation.style != .completed {
                    GitOutcomeCard(presentation: presentation, primary: {
                        Task { await repository.perform(presentation.nextStep) }
                    }, dismiss: {
                        Task { await repository.acknowledgeOutcome() }
                    })
                    .padding(.horizontal, 12)
                    .padding(.top, 72)
                    .shadow(radius: 12)
                }
            }
            .confirmationDialog(
                repository.conflictMarkersQuestion ?? "",
                isPresented: Binding(
                    get: { repository.conflictMarkersQuestion != nil },
                    set: { shown in if !shown, repository.conflictMarkersQuestion != nil { repository.declineConflictMarkers() } }
                ),
                titleVisibility: .visible
            ) {
                Button("Stage anyway") { Task { await repository.confirmConflictMarkers() } }
                Button("Cancel", role: .cancel) { repository.declineConflictMarkers() }
            } message: {
                Text("Staging marks the file resolved. Latch doesn't edit files; resolve them on your Mac or in a terminal session.")
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

    @ViewBuilder
    private var content: some View {
        if let diff, let page = diff.diff {
            if let explanation = GitPresentation.noTextExplanation(page.kind) {
                MessageView(icon: explanation.icon, title: explanation.title, detail: explanation.detail)
            } else if diff.hunks.isEmpty {
                MessageView(
                    icon: "equal.circle",
                    title: "No line changes",
                    detail: page.notes.isEmpty
                        ? "Only this file's name or mode changed."
                        : page.notes.joined(separator: "\n")
                )
            } else {
                hunks(diff, page: page)
                    .opacity(diff.isStale ? 0.55 : 1)
            }
        } else if let error = diff?.error {
            VStack(spacing: 12) {
                MessageView(icon: "exclamationmark.triangle", title: "Cannot show this diff", detail: error)
                Button("Try again") { Task { await diff?.load() } }
                    .buttonStyle(.bordered)
                    .padding(.bottom, 24)
            }
        } else {
            ProgressView("Reading the diff…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func hunks(_ diff: GitDiffModel, page: GitFileDiff) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                if diff.isStale {
                    GitStalePill(text: repository.stalePill())
                        .frame(maxWidth: .infinity)
                }
                if let notice = diff.notice {
                    Text(notice).font(.footnote).foregroundStyle(.secondary)
                }
                if let origPath = page.origPath {
                    Text("Renamed from \(origPath)")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                ForEach(page.notes, id: \.self) { note in
                    Text(note).font(.caption.monospaced()).foregroundStyle(.secondary)
                }
                ForEach(diff.hunks) { hunk in
                    GitHunkView(hunk: hunk)
                }
                if let progress = diff.progressText {
                    VStack(spacing: 8) {
                        Text(progress)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                        Button {
                            Task { await diff.loadMore() }
                        } label: {
                            if diff.isLoading { ProgressView() } else { Text("Load more") }
                        }
                        .buttonStyle(.bordered)
                        .disabled(diff.isLoading || !appModel.linkState.isUsable)
                    }
                    .padding(14)
                    .frame(maxWidth: .infinity)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                    .accessibilityElement(children: .contain)
                }
                if let error = diff.error {
                    Text(error).font(.footnote).foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
    }

    private var bottomBar: some View {
        let previous = repository.neighbor(of: entry, offset: -1)
        let next = repository.neighbor(of: entry, offset: 1)
        return HStack(spacing: 12) {
            Button {
                if let previous { entry = previous }
            } label: {
                FloatingControl(systemImage: "chevron.up").foregroundStyle(.primary)
            }
            .disabled(previous == nil)
            .accessibilityLabel("Previous changed file")
            Spacer(minLength: 0)
            if repository.offersStaging(entry) {
                let enabled = repository.canToggleStaging(entry)
                Button {
                    guard enabled else {
                        blockedReason = repository.mutationBlockReason
                        return
                    }
                    Task { await repository.toggleStaging(entry) }
                } label: {
                    FloatingControl(shape: .capsule) {
                        HStack(spacing: 6) {
                            if repository.mutations.phase.isWorking { ProgressView() }
                            Text(entry.area == .staged ? "Unstage file" : entry.area == .conflicted ? "Stage as resolved" : "Stage file")
                                .font(.body.weight(.semibold))
                        }
                    }
                    .foregroundStyle(enabled ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
                }
                .accessibilityLabel(entry.stagingLabel)
                .accessibilityIdentifier("diff.stage")
            }
            Spacer(minLength: 0)
            Button {
                if let next { entry = next }
            } label: {
                FloatingControl(systemImage: "chevron.down").foregroundStyle(.primary)
            }
            .disabled(next == nil)
            .accessibilityLabel("Next changed file")
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background { BottomFade() }
    }
}

/// One hunk: its header, then each line with both numbers and its sign.
struct GitHunkView: View {
    let hunk: GitDiffHunk
    @ScaledMetric(relativeTo: .caption) private var numberWidth: CGFloat = 34
    @ScaledMetric(relativeTo: .caption) private var signWidth: CGFloat = 10

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(hunk.header)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.accentColor.opacity(0.08))
                .accessibilityLabel("Hunk starting at line \(hunk.newStart)")
            ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, line in
                lineRow(line)
            }
            if hunk.omittedLines > 0 {
                Text("\(hunk.omittedLines) more lines in this hunk are too many to show on a phone.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(8)
            }
        }
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        // Code wrapped a word per line is unreadable; past this size the
        // lines stop growing, as the terminal's do.
        .dynamicTypeSize(...DynamicTypeSize.xxxLarge)
    }

    private func lineRow(_ line: GitDiffLine) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(line.oldLine.map(String.init) ?? " ")
                .frame(width: numberWidth, alignment: .trailing)
                .foregroundStyle(.tertiary)
            Text(line.newLine.map(String.init) ?? " ")
                .frame(width: numberWidth, alignment: .trailing)
                .foregroundStyle(.tertiary)
            // Fixed so a wide sign glyph never wraps to an empty second line.
            Text(sign(line.kind))
                .fixedSize()
                .frame(width: signWidth)
                .foregroundStyle(signColor(line.kind))
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(line.kind == .note ? .secondary : .primary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .font(.caption.monospaced())
        .padding(.horizontal, 8)
        .padding(.vertical, 1)
        .background(background(line.kind))
        .textSelection(.enabled)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken(line))
    }

    private func sign(_ kind: GitDiffLineKind) -> String {
        switch kind {
        case .added: "+"
        case .removed: "\u{2212}"
        case .context, .note: " "
        }
    }

    private func signColor(_ kind: GitDiffLineKind) -> Color {
        switch kind {
        case .added: .green
        case .removed: .red
        case .context, .note: .secondary
        }
    }

    private func background(_ kind: GitDiffLineKind) -> Color {
        switch kind {
        case .added: .green.opacity(0.12)
        case .removed: .red.opacity(0.12)
        case .context, .note: .clear
        }
    }

    private func spoken(_ line: GitDiffLine) -> String {
        switch line.kind {
        case .added: "Added line \(line.newLine ?? 0): \(line.text)"
        case .removed: "Removed line \(line.oldLine ?? 0): \(line.text)"
        case .context: "Line \(line.newLine ?? 0): \(line.text)"
        case .note: line.text
        }
    }
}
