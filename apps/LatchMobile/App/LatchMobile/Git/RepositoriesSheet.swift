import LatchMobileKit
import SwiftUI

/// Where a repository is opened from the session list: the working trees of
/// running sessions, the repositories this Mac opened recently, or a folder
/// chosen in the browser. The Mac keeps the recent list, so every phone
/// paired with it sees the same one.
struct RepositoriesSheet: View {
    /// Receives the chosen folder; the presenter opens it once this sheet
    /// has gone.
    let choose: (String) -> Void

    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var list: GitRepositoryListModel?
    @State private var isOpening = true
    @State private var openFailure: String?
    @State private var browsing = false

    var body: some View {
        NavigationStack {
            Group {
                if let list {
                    listing(list)
                } else if isOpening {
                    ProgressView("Checking…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    MessageView(
                        icon: "lock",
                        title: "Not unlocked",
                        detail: openFailure ?? RepositoryView.deniedDetail
                    )
                }
            }
            .navigationTitle("Repositories")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .sheet(isPresented: $browsing) {
            FolderPickerView(mode: .chooseRepository) { repository in
                browsing = false
                pick(repository.root)
            }
        }
        .task {
            guard list == nil else { return }
            list = await model.gitRepositoryList()
            openFailure = model.ownerAuthenticationFailure
            isOpening = false
        }
    }

    private func listing(_ list: GitRepositoryListModel) -> some View {
        List {
            if let explanation = model.gitUnavailableExplanation {
                Text(explanation).font(.footnote).foregroundStyle(.secondary)
            }
            if let repositories = list.list?.fromSessions, !repositories.isEmpty {
                Section("From open sessions") {
                    ForEach(repositories) { repository in row(repository) }
                }
            }
            if let repositories = list.list?.recent, !repositories.isEmpty {
                Section("Recent on this Mac") {
                    ForEach(repositories) { repository in row(repository) }
                }
            }
            if list.isEmpty {
                Text("No session on your Mac is in a repository, and none was opened recently.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            if list.list == nil, list.isLoading {
                HStack { ProgressView(); Text("Loading…").foregroundStyle(.secondary) }
            }
            if let error = list.error {
                Text(error).font(.footnote).foregroundStyle(.secondary)
            }
            Section {
                Button {
                    browsing = true
                } label: {
                    Label("Browse folders on your Mac…", systemImage: "folder")
                }
                .disabled(!model.canBrowseNewSessionFolders)
                .accessibilityIdentifier("repositories.browse")
            }
        }
        .refreshable { await list.load() }
    }

    private func row(_ repository: GitRepositorySummary) -> some View {
        Button {
            pick(repository.root)
        } label: {
            HStack(spacing: 12) {
                Image(systemName: repository.kind == .submodule ? "shippingbox" : "arrow.triangle.branch")
                    .foregroundStyle(.secondary)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 2) {
                    Text(repository.name)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                    Text(repository.root)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Spacer(minLength: 8)
                if !repository.sessions.isEmpty {
                    Text("\(repository.sessions.count)")
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 8)
                        .padding(.vertical, 2)
                        .background(
                            (repository.sessions.activeCount > 0 ? Color.orange : Color.secondary).opacity(0.18),
                            in: Capsule()
                        )
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(repository.name)
        .accessibilityValue(sessionsSpoken(repository))
        .accessibilityHint("Opens this repository. It does not open a terminal.")
    }

    private func sessionsSpoken(_ repository: GitRepositorySummary) -> String {
        let count = repository.sessions.count
        guard count > 0 else { return repository.root }
        return "\(count) \(count == 1 ? "session" : "sessions") here, \(repository.root)"
    }

    private func pick(_ folder: String) {
        choose(folder)
        dismiss()
    }
}
