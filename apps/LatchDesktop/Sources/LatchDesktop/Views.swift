import SwiftUI
import AppKit

struct SessionsView: View {
    @ObservedObject var store: SessionStore
    @State private var showingCreate = false
    @State private var showingPrune = false

    var body: some View {
        NavigationSplitView {
            List(store.filteredSessions, selection: $store.selection) { session in
                SessionRow(session: session)
                    .tag(session.id)
                    .contextMenu {
                        Button("Open in \(store.preferredTerminal.rawValue)") {
                            Task { await store.open(session.id) }
                        }
                    }
            }
            .searchable(text: $store.search, prompt: "Search sessions")
            .overlay {
                if store.sessions.isEmpty && !store.isRefreshing {
                    EmptyStateView(
                        title: "No Latch Sessions",
                        systemImage: "rectangle.stack.badge.plus",
                        message: "Create a persistent shell session to get started."
                    )
                }
            }
            .navigationTitle("Sessions")
            .toolbar {
                ToolbarItemGroup {
                    Menu {
                        Button("All") { store.stateFilter = nil }
                        Divider()
                        ForEach(SessionState.allCases, id: \.self) { state in
                            Button(state.rawValue.capitalized) { store.stateFilter = state }
                        }
                    } label: {
                        Label("Filter", systemImage: "line.3.horizontal.decrease.circle")
                    }
                    Button { showingCreate = true } label: {
                        Label("New Session", systemImage: "plus")
                    }
                    .keyboardShortcut("n")
                }
            }
        } detail: {
            if let details = store.details, details.id == store.selection {
                SessionDetailView(store: store, session: details)
            } else {
                EmptyStateView(
                    title: "Select a Session",
                    systemImage: "rectangle.stack",
                    message: "Choose a session to inspect and manage it."
                )
            }
        }
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    Task { await store.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(store.isRefreshing)
                Button("Prune…") {
                    Task {
                        await store.previewPrune()
                        showingPrune = store.prunePreview != nil
                    }
                }
            }
        }
        .onChange(of: store.selection) { _ in Task { await store.loadDetails() } }
        .onAppear { handleMenuRequests() }
        .onChange(of: store.shouldPresentNewSession) { requested in
            if requested { handleMenuRequests() }
        }
        .onChange(of: store.shouldPresentPrune) { requested in
            if requested { handleMenuRequests() }
        }
        .sheet(isPresented: $showingCreate) {
            NewSessionView(store: store, isPresented: $showingCreate)
        }
        .sheet(isPresented: $showingPrune) {
            PruneView(store: store, isPresented: $showingPrune)
        }
        .alert("Latch", isPresented: errorBinding) {
            Button("OK") { store.errorMessage = nil }
        } message: {
            Text(store.errorMessage ?? "")
        }
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { store.errorMessage != nil },
            set: { if !$0 { store.errorMessage = nil } }
        )
    }

    private func handleMenuRequests() {
        if store.shouldPresentNewSession {
            store.shouldPresentNewSession = false
            showingCreate = true
        }
        if store.shouldPresentPrune {
            store.shouldPresentPrune = false
            Task {
                await store.previewPrune()
                showingPrune = store.prunePreview != nil
            }
        }
    }
}

private struct EmptyStateView: View {
    let title: String
    let systemImage: String
    let message: String

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage).font(.system(size: 34)).foregroundStyle(.secondary)
            Text(title).font(.headline)
            Text(message).font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
        }
        .padding()
    }
}

private struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(stateColor)
                .frame(width: 9, height: 9)
                .accessibilityLabel(session.state.rawValue)
            VStack(alignment: .leading, spacing: 2) {
                Text(session.name).fontWeight(.medium).lineLimit(1)
                Text(session.title ?? "\(session.commandLabel) — \(session.cwd)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            Text(session.state.rawValue)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }

    private var stateColor: Color {
        switch session.state {
        case .running: return .green
        case .creating, .stopping: return .orange
        case .exited: return .secondary
        case .lost: return .red
        }
    }
}

private struct SessionDetailView: View {
    @ObservedObject var store: SessionStore
    let session: InspectReport
    @State private var renameValue = ""
    @State private var showingRename = false
    @State private var destructiveAction: DestructiveAction?

    enum DestructiveAction: String, Identifiable {
        case stop, remove, forceRemove
        var id: String { rawValue }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(session.name).font(.largeTitle).fontWeight(.semibold)
                        if let title = session.title { Text(title).foregroundStyle(.secondary) }
                    }
                    Spacer()
                    Text(session.state.rawValue.capitalized)
                        .padding(.horizontal, 9).padding(.vertical, 4)
                        .background(.quaternary, in: Capsule())
                }

                Button {
                    Task { await store.open(session.id) }
                } label: {
                    Label("Open in \(store.preferredTerminal.rawValue)", systemImage: "terminal")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

                Grid(alignment: .leading, horizontalSpacing: 22, verticalSpacing: 10) {
                    DetailRow(label: "Command", value: session.commandLabel)
                    DetailRow(label: "Directory", value: session.cwd)
                    DetailRow(label: "Created", value: session.createdAt)
                    DetailRow(label: "Size", value: sizeDescription)
                    DetailRow(label: "Session ID", value: session.id)
                    if let exit = session.exit {
                        DetailRow(label: "Exit", value: exitDescription(exit))
                    }
                }
                .textSelection(.enabled)

                if let attachments = session.attachments, !attachments.isEmpty {
                    GroupBox("Attachments") {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(attachments) { attachment in
                                Label(
                                    "\(attachment.clientName) (\(attachment.mode))",
                                    systemImage: attachment.mode == "control" ? "keyboard" : "eye"
                                )
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }

                HStack {
                    Button("Rename…") {
                        renameValue = session.name
                        showingRename = true
                    }
                    if session.state.isLive {
                        Button("Stop…") { destructiveAction = .stop }
                    }
                    Spacer()
                    Button("Remove…", role: .destructive) {
                        destructiveAction = session.state.isLive ? .forceRemove : .remove
                    }
                }
            }
            .padding(28)
        }
        .navigationTitle(session.name)
        .alert("Rename Session", isPresented: $showingRename) {
            TextField("Name", text: $renameValue)
            Button("Cancel", role: .cancel) {}
            Button("Rename") { Task { await store.rename(session.id, to: renameValue) } }
        }
        .confirmationDialog(
            destructiveTitle,
            isPresented: Binding(
                get: { destructiveAction != nil },
                set: { if !$0 { destructiveAction = nil } }
            ),
            titleVisibility: .visible
        ) {
            destructiveButtons
            Button("Cancel", role: .cancel) { destructiveAction = nil }
        } message: {
            Text(destructiveMessage)
        }
    }

    @ViewBuilder private var destructiveButtons: some View {
        switch destructiveAction {
        case .stop:
            Button("Stop", role: .destructive) { Task { await store.stop(session.id, force: false) } }
            Button("Force Stop", role: .destructive) { Task { await store.stop(session.id, force: true) } }
        case .remove:
            Button("Remove", role: .destructive) { Task { await store.remove(session.id, force: false) } }
        case .forceRemove:
            Button("Stop and Remove", role: .destructive) { Task { await store.remove(session.id, force: true) } }
        case nil:
            EmptyView()
        }
    }

    private var destructiveTitle: String {
        switch destructiveAction {
        case .stop: return "Stop \(session.name)?"
        case .remove, .forceRemove: return "Remove \(session.name)?"
        case nil: return "Manage Session"
        }
    }

    private var destructiveMessage: String {
        switch destructiveAction {
        case .stop:
            return "Stopping ends the child process but retains its final screen for later inspection."
        case .remove:
            return "This permanently deletes the retained screen and session metadata."
        case .forceRemove:
            return "This stops the live child process, then permanently deletes its retained screen and metadata."
        case nil: return ""
        }
    }

    private var sizeDescription: String {
        let size = session.size ?? session.initialSize
        return "\(size.cols) × \(size.rows)"
    }

    private func exitDescription(_ exit: ExitRecord) -> String {
        if let code = exit.code { return "Code \(code) at \(exit.exitedAt)" }
        return "\(exit.signal ?? "signal") at \(exit.exitedAt)"
    }
}

private struct DetailRow: View {
    let label: String
    let value: String

    var body: some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct NewSessionView: View {
    @ObservedObject var store: SessionStore
    @Binding var isPresented: Bool
    @State private var request = NewSessionRequest()
    @State private var advanced = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("New Session").font(.title2).fontWeight(.semibold)
            Form {
                TextField("Name (optional)", text: $request.name)
                TextField("Title (optional)", text: $request.title)
                TextField("Working directory", text: $request.cwd)
                DisclosureGroup("Advanced", isExpanded: $advanced) {
                    TextField("Command (optional)", text: $request.command)
                    HStack {
                        TextField("Columns", value: $request.cols, format: .number)
                        TextField("Rows", value: $request.rows, format: .number)
                    }
                }
            }
            HStack {
                Button("Cancel", role: .cancel) { isPresented = false }
                Spacer()
                Button("Create") { submit(open: false) }
                Button("Create and Open") { submit(open: true) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(24)
        .frame(width: 480)
    }

    private func submit(open: Bool) {
        let submitted = request
        isPresented = false
        Task { await store.create(submitted, openAfterCreation: open) }
    }
}

private struct PruneView: View {
    @ObservedObject var store: SessionStore
    @Binding var isPresented: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Prune Sessions").font(.title2).fontWeight(.semibold)
            if let report = store.prunePreview {
                if report.reclaimed.isEmpty {
                    Text("There are no exited or lost sessions to reclaim.")
                } else {
                    Text("The retained screen and metadata for \(report.reclaimed.count) session(s) will be permanently deleted:")
                    List(report.reclaimed, id: \.self) { Text($0).textSelection(.enabled) }
                        .frame(height: 180)
                }
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { isPresented = false }
                if store.prunePreview?.reclaimed.isEmpty == false {
                    Button("Prune", role: .destructive) {
                        isPresented = false
                        Task { await store.pruneAll() }
                    }
                }
            }
        }
        .padding(24)
        .frame(width: 520)
    }
}

struct MenuBarSessionsView: View {
    @ObservedObject var store: SessionStore
    let openMainWindow: () -> Void
    let openSettings: () -> Void

    var body: some View {
        Text("\(store.runningCount) running session\(store.runningCount == 1 ? "" : "s")")
        Divider()
        ForEach(store.sessions.prefix(6)) { session in
            Button {
                Task { await store.open(session.id) }
            } label: {
                Label(session.name, systemImage: session.state == .running ? "circle.fill" : "circle")
            }
        }
        if store.sessions.isEmpty { Text("No sessions") }
        Divider()
        Button("New Session…") {
            store.shouldPresentNewSession = true
            openMainWindow()
        }
        Button("Prune…") {
            store.shouldPresentPrune = true
            openMainWindow()
        }
        Button("Refresh") { Task { await store.refresh() } }
        Button("Open Latch") { openMainWindow() }
        Button("Settings…") { openSettings() }
        Divider()
        Button("Quit Latch") { NSApp.terminate(nil) }
    }
}

struct SettingsView: View {
    @ObservedObject var store: SessionStore

    var body: some View {
        Form {
            Section("Latch CLI") {
                TextField("Latch executable", text: $store.latchExecutablePath)
                    .textFieldStyle(.roundedBorder)
                Text("Leave blank to use the CLI bundled with the app, then Homebrew or /usr/local.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Picker("Preferred terminal", selection: Binding(
                get: { store.preferredTerminal },
                set: { store.preferredTerminal = $0 }
            )) {
                ForEach(PreferredTerminal.allCases) { terminal in
                    Text(terminal.rawValue + (TerminalLauncher.isInstalled(terminal) ? "" : " (not installed)"))
                        .tag(terminal)
                }
            }
            if store.preferredTerminal == .custom {
                Button("Choose Terminal Application…") {
                    chooseCustomTerminalApplication()
                }
                TextField("Terminal executable", text: $store.customTerminalExecutable)
                TextField("Argument template", text: $store.customTerminalTemplate)
                Text("Choose any .app not listed above, then adjust its launch arguments if needed. Required placeholders: {latch} and {session}. Arguments are parsed directly and never passed to a shell.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Text("This is the default app Latch uses when opening a session. Closing Latch never stops sessions.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(width: 460)
    }

    private func chooseCustomTerminalApplication() {
        let panel = NSOpenPanel()
        panel.title = "Choose Terminal Application"
        panel.message = "Select the application Latch should use to open session attachments."
        panel.prompt = "Choose"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedFileTypes = ["app"]

        guard panel.runModal() == .OK, let applicationURL = panel.url else { return }
        do {
            store.customTerminalExecutable = try TerminalLauncher.executablePath(forApplicationURL: applicationURL)
        } catch {
            store.errorMessage = error.localizedDescription
        }
    }
}
