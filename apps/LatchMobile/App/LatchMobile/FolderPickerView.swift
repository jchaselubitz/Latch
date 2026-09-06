import LatchMobileKit
import SwiftUI

/// The remote folder browser, shared by **New session** and
/// **Default folder**.
///
/// It browses the Mac, not the phone: the iOS document picker can reach this
/// device's files and its cloud providers, and can reach none of the folders
/// the Mac would actually run a shell in. Every listing here arrives over the
/// same paired tunnel as the session list.
struct FolderPickerView: View {
    let mode: FolderBrowserMode

    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    /// Built by the model only after the device-owner check passes, so no
    /// folder name is fetched before the person at the phone is confirmed.
    @State private var browser: FolderBrowserModel?
    @State private var openFailure: String?
    @State private var isOpening = true

    var body: some View {
        NavigationStack {
            Group {
                if let browser {
                    browsing(browser)
                } else if isOpening {
                    ProgressView("Checking…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    MessageView(
                        icon: "lock",
                        title: "Not unlocked",
                        detail: openFailure ?? Self.deniedDetail
                    )
                }
            }
            .navigationTitle(mode == .create ? "New session" : "Default folder")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        browser?.cancelCreation()
                        dismiss()
                    }
                }
            }
        }
        .task {
            guard browser == nil else { return }
            browser = await model.newSessionFolderBrowser(mode: mode)
            openFailure = model.ownerAuthenticationFailure
            isOpening = false
        }
    }

    @ViewBuilder
    private func browsing(_ browser: FolderBrowserModel) -> some View {
        VStack(spacing: 0) {
            pathHeader(browser)
            // A grant can be withdrawn while this sheet is up. The browser
            // refuses every further request on its own; this says why the
            // screen stopped working, rather than leaving a stale listing
            // that quietly does nothing.
            if let explanation = model.newSessionUnavailableExplanation {
                BannerView(text: explanation)
            } else if let notice = browser.notice {
                BannerView(text: notice)
            }
            listing(browser)
            actionBar(browser)
        }
        .onChange(of: browser.createdSessionID) { _, created in
            if created != nil { dismiss() }
        }
    }

    /// The absolute path, in full. Two folders named `src` are told apart by
    /// nothing else, so this is not decoration.
    @ViewBuilder
    private func pathHeader(_ browser: FolderBrowserModel) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Current folder")
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(browser.currentPage?.path ?? "…")
                .font(.footnote.monospaced())
                .textSelection(.enabled)
                .lineLimit(3)
                .truncationMode(.head)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.bar)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Current folder, \(browser.currentPage?.path ?? "loading")")
    }

    @ViewBuilder
    private func listing(_ browser: FolderBrowserModel) -> some View {
        if browser.currentPage == nil, let error = browser.error {
            unreachable(browser, error: error)
        } else if browser.currentPage == nil {
            ProgressView("Loading folders…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            List {
                if let parent = browser.currentPage?.parent {
                    Button {
                        Task { await browser.navigate(to: parent) }
                    } label: {
                        Label(Self.displayName(of: parent), systemImage: "arrow.turn.left.up")
                    }
                    .accessibilityLabel("Go up to \(parent)")
                }

                if browser.entries.isEmpty {
                    Text("No folders here")
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("This folder contains no subfolders")
                }

                ForEach(browser.entries, id: \.path) { entry in
                    Button {
                        Task { await browser.navigate(to: entry.path) }
                    } label: {
                        HStack {
                            Label(entry.name, systemImage: "folder")
                                .lineLimit(2)
                            Spacer(minLength: 8)
                            Image(systemName: "chevron.right")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                        }
                    }
                    .accessibilityLabel("Folder \(entry.name)")
                    .accessibilityHint("Opens this folder")
                    // A directory with thousands of children arrives a page at
                    // a time; reaching the end of one asks for the next.
                    .onAppear {
                        guard entry.path == browser.entries.last?.path else { return }
                        Task { await browser.loadMore() }
                    }
                }

                if browser.isLoadingMore {
                    HStack {
                        ProgressView()
                        Text("Loading more…").foregroundStyle(.secondary)
                    }
                }
            }
            .listStyle(.plain)
            .disabled(browser.isLoading)
            // A transient failure keeps the folder that is already on screen.
            // Losing a valid listing to a dropped connection would make the
            // person navigate back to where they already were.
            .overlay(alignment: .bottom) {
                if let error = browser.error {
                    inlineRetry(browser, error: error)
                }
            }
        }
    }

    @ViewBuilder
    private func unreachable(_ browser: FolderBrowserModel, error: String) -> some View {
        VStack(spacing: 12) {
            MessageView(
                icon: "exclamationmark.triangle",
                title: "Cannot list folders",
                detail: error
            )
            Button("Try again") { Task { await browser.retry() } }
                .buttonStyle(.bordered)
                .padding(.bottom, 24)
        }
    }

    @ViewBuilder
    private func inlineRetry(_ browser: FolderBrowserModel, error: String) -> some View {
        HStack(spacing: 12) {
            Text(error)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 8)
            Button("Retry") { Task { await browser.retry() } }
                .font(.footnote.weight(.semibold))
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity)
        .background(.thinMaterial)
    }

    @ViewBuilder
    private func actionBar(_ browser: FolderBrowserModel) -> some View {
        VStack(spacing: 8) {
            Button {
                switch mode {
                case .create:
                    Task { await browser.startSession() }
                case .chooseDefault:
                    if browser.useCurrentAsDefault() {
                        model.reloadDefaultNewSessionFolder()
                        dismiss()
                    }
                }
            } label: {
                HStack {
                    if browser.isLoading, mode == .create {
                        ProgressView()
                    }
                    Text(mode == .create ? "Start session here" : "Use as default")
                        .font(.body.weight(.semibold))
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(browser.currentPage == nil || browser.isLoading || !isPermitted)

            if mode == .create {
                Text("Starts a shell here. It does not open the session or run an agent.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
            }
        }
        .padding(16)
        .background(.bar)
    }

    private var isPermitted: Bool {
        mode == .create ? model.canCreateNewSession : model.canBrowseNewSessionFolders
    }

    /// The last component of an absolute path, with the filesystem root shown
    /// as itself rather than as an empty name.
    static func displayName(of path: String) -> String {
        let name = (path as NSString).lastPathComponent
        return name.isEmpty || name == "/" ? path : name
    }

    static let deniedDetail = """
    Latch needs Face ID, Touch ID, or your passcode before it shows folders on your Mac or \
    starts anything on it. Try again when you're ready.
    """
}
