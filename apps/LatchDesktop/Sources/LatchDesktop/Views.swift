import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @State private var showingCreate = false
    @State private var showingPrune = false

    var body: some View {
        NavigationSplitView {
            List(store.filteredSessions, selection: $store.selection) { session in
                SessionRow(session: session)
                    .tag(session.id)
                    .contextMenu {
                        let canOpen = session.state.isAttachable && store.canAttachSessions
                        Button("Open in \(store.preferredTerminal.rawValue)") {
                            Task { await store.open(session.id) }
                        }
                        .disabled(!canOpen)
                        Divider()
                        OpenBehaviorMenuItems(
                            store: store,
                            sessionID: session.id,
                            isEnabled: canOpen
                        )
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
                    .disabled(!store.canCreateSessions)
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
        // Without this the toolbar's sidebar section stops short of the split divider, so
        // the sidebar's trailing border only exists below the toolbar.
        .background(SidebarToolbarSeparator())
        // Carries the sidebar's material across the rest of the titlebar so the header reads
        // as one surface, and steps aside when the sidebar is collapsed.
        .background(TitlebarSidebarMaterial())
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
        .sheet(isPresented: $updates.isPresented) {
            UpdateView(updates: updates)
        }
        .sheet(isPresented: $store.shouldPresentCLISetup) {
            CLISetupView(store: store)
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

/// A split button: clicking it opens the session the way Settings says, while the
/// attached menu offers every launch shape the preferred terminal understands.
private struct OpenSessionButton: View {
    @ObservedObject var store: SessionStore
    let sessionID: String
    let isEnabled: Bool

    var body: some View {
        Menu {
            OpenBehaviorMenuItems(store: store, sessionID: sessionID, isEnabled: isEnabled)
        } label: {
            Label("Open in \(store.preferredTerminal.rawValue)", systemImage: "terminal")
                .frame(maxWidth: .infinity)
        } primaryAction: {
            Task { await store.open(sessionID) }
        }
        .menuStyle(.button)
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .frame(maxWidth: .infinity)
        .disabled(!isEnabled)
    }
}

/// The launch shapes offered by every Open control. Unavailable shapes stay visible
/// with the reason attached rather than disappearing.
private struct OpenBehaviorMenuItems: View {
    @ObservedObject var store: SessionStore
    let sessionID: String
    let isEnabled: Bool

    var body: some View {
        if store.preferredTerminal.supportedOpenBehaviors.isEmpty {
            Text("Your argument template decides how this terminal opens.")
        } else {
            ForEach(TerminalOpenBehavior.allCases) { behavior in
                Button {
                    Task { await store.open(sessionID, behavior: behavior) }
                } label: {
                    Label(title(for: behavior), systemImage: behavior.systemImage)
                }
                .disabled(!isEnabled || !store.preferredTerminal.supports(behavior))
            }
        }
    }

    private func title(for behavior: TerminalOpenBehavior) -> String {
        if let reason = store.preferredTerminal.unsupportedReason(for: behavior) {
            return "\(behavior.label) — \(reason)"
        }
        return behavior == store.effectiveOpenBehavior ? "\(behavior.label) (Default)" : behavior.label
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
            VStack(alignment: .trailing, spacing: 2) {
                Text(session.state.rawValue)
                if let idle = idleDescription {
                    Text(idle)
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }

    private var stateColor: Color {
        switch session.state {
        case .running: return .green
        case .exited: return .secondary
        case .lost: return .red
        }
    }

    private var idleDescription: String? {
        guard let milliseconds = session.idleMs else { return nil }
        let seconds = milliseconds / 1_000
        if seconds < 60 { return "\(seconds)s idle" }
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m idle" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h idle" }
        return "\(hours / 24)d idle"
    }
}

private struct SessionDetailView: View {
    @ObservedObject var store: SessionStore
    let session: InspectReport
    @State private var renameValue = ""
    @State private var showingRename = false
    @State private var showingResize = false
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

                OpenSessionButton(
                    store: store,
                    sessionID: session.id,
                    isEnabled: session.state.isAttachable && store.canAttachSessions
                )

                Grid(alignment: .leading, horizontalSpacing: 22, verticalSpacing: 10) {
                    DetailRow(label: "Command", value: session.commandLabel)
                    DetailRow(label: "Directory", value: session.cwd)
                    DetailRow(label: "Created", value: session.createdAt)
                    if let summary = store.sessions.first(where: { $0.id == session.id }),
                       let lastActivity = summary.lastActivityAt {
                        DetailRow(label: "Last Activity", value: lastActivity)
                    }
                    DetailRow(label: "Size", value: sizeDescription)
                    DetailRow(label: "Session ID", value: session.id)
                    if let exit = session.exit {
                        DetailRow(label: "Exit", value: exitDescription(exit))
                    }
                    if let attached = session.attached {
                        DetailRow(label: "Attached", value: "\(attached)")
                    }
                }
                .textSelection(.enabled)

                HStack {
                    Button("Rename…") {
                        renameValue = session.name
                        showingRename = true
                    }
                    if session.state == .running {
                        Button("Resize…") { showingResize = true }
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
        .sheet(isPresented: $showingResize) {
            ResizeSessionView(
                store: store,
                session: session,
                isPresented: $showingResize
            )
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

private struct ResizeSessionView: View {
    @ObservedObject var store: SessionStore
    let session: InspectReport
    @Binding var isPresented: Bool
    @State private var request: ResizeSessionRequest

    init(store: SessionStore, session: InspectReport, isPresented: Binding<Bool>) {
        self.store = store
        self.session = session
        _isPresented = isPresented
        let size = session.size ?? session.initialSize
        _request = State(initialValue: ResizeSessionRequest(cols: size.cols, rows: size.rows))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Resize \(session.name)")
                .font(.title2)
                .fontWeight(.semibold)
            Form {
                TextField("Columns", value: $request.cols, format: .number)
                TextField("Rows", value: $request.rows, format: .number)
                Toggle("Pin this size", isOn: $request.pin)
                Text("Without pinning, an attached terminal can change the session size later. Pinning switches the tmux window to manual sizing.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            HStack {
                Button("Cancel", role: .cancel) { isPresented = false }
                Spacer()
                Button("Resize") {
                    let submitted = request
                    isPresented = false
                    Task { await store.resize(session.id, request: submitted) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(request.cols == 0 || request.rows == 0)
            }
        }
        .padding(24)
        .frame(width: 420)
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

/// The one place an update is described and accepted.
///
/// It states the version, links the release notes rather than reproducing
/// them, and never installs without being asked — an app that replaces itself
/// while somebody is working in it is worse than an app that is a day old.
private struct UpdateView: View {
    @ObservedObject var updates: UpdateController

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(title).font(.title2).fontWeight(.semibold)
            content
            HStack {
                Spacer()
                buttons
            }
        }
        .padding(24)
        .frame(width: 460)
    }

    private var title: String {
        switch updates.phase {
        case .available: return "Update Available"
        case .failed: return "Update Failed"
        case .upToDate: return "Latch Is Up to Date"
        default: return "Checking for Updates"
        }
    }

    @ViewBuilder private var content: some View {
        switch updates.phase {
        case .idle, .checking:
            ProgressView().progressViewStyle(.linear)
        case .upToDate:
            Text("Latch \(updates.installedVersionLabel) is the newest release.")
        case .available(let update):
            VStack(alignment: .leading, spacing: 10) {
                Text("Latch \(update.version.description) is available. You have \(updates.installedVersionLabel).")
                Link("Release notes", destination: update.releasePage)
            }
        case .downloading:
            VStack(alignment: .leading, spacing: 10) {
                Text("Downloading and verifying the update…")
                ProgressView().progressViewStyle(.linear)
            }
        case .failed(let message):
            Text(message).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var buttons: some View {
        switch updates.phase {
        case .available(let update):
            Button("Not Now") { updates.dismiss() }
            Button("Install and Relaunch") {
                Task { await updates.install(update) }
            }
            .buttonStyle(.borderedProminent)
        case .downloading:
            Button("Cancel", role: .cancel) { updates.dismiss() }.disabled(true)
        default:
            Button("OK") { updates.dismiss() }
        }
    }
}

struct MenuBarSessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    let openMainWindow: () -> Void
    let openSettings: () -> Void
    let checkForUpdates: () -> Void

    var body: some View {
        Text("\(store.runningCount) running session\(store.runningCount == 1 ? "" : "s")")
        if let update = updates.pendingUpdate {
            Button("Update to Latch \(update.version.description)…") { checkForUpdates() }
        }
        Divider()
        ForEach(store.sessions.prefix(6)) { session in
            Button {
                Task { await store.open(session.id) }
            } label: {
                Label(session.name, systemImage: session.state == .running ? "circle.fill" : "circle")
            }
            .disabled(!session.state.isAttachable || !store.canAttachSessions)
        }
        if store.sessions.isEmpty { Text("No sessions") }
        Divider()
        Button("New Session…") {
            store.shouldPresentNewSession = true
            openMainWindow()
        }
        .disabled(!store.canCreateSessions)
        Button("Prune…") {
            store.shouldPresentPrune = true
            openMainWindow()
        }
        Button("Refresh") { Task { await store.refresh() } }
        Button("Open Latch") { openMainWindow() }
        Button("Check for Updates…") { checkForUpdates() }
        Button("Settings…") { openSettings() }
        Divider()
        Button("Quit Latch") { NSApp.terminate(nil) }
    }
}

struct SettingsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @ObservedObject var remoteAccess: RemoteAccessController

    private enum Tab: String, Hashable {
        case terminal
        case remoteAccess
        case cli
        case updates
    }

    @State private var selection: Tab = .terminal

    var body: some View {
        TabView(selection: $selection) {
            terminalTab
                .tabItem { Label("Terminal", systemImage: "terminal") }
                .tag(Tab.terminal)
            RemoteAccessSettingsView(controller: remoteAccess)
                .tabItem { Label("Remote Access", systemImage: "iphone.and.arrow.forward") }
                .tag(Tab.remoteAccess)
            cliTab
                .tabItem { Label("Latch CLI", systemImage: "wrench.and.screwdriver") }
                .tag(Tab.cli)
            updatesTab
                .tabItem { Label("Updates", systemImage: "arrow.down.circle") }
                .tag(Tab.updates)
        }
        .frame(width: 580, height: 470)
    }

    // MARK: - Terminal

    private var terminalTab: some View {
        Form {
            Section {
                Picker("Preferred terminal", selection: Binding(
                    get: { store.preferredTerminal },
                    set: { store.preferredTerminal = $0 }
                )) {
                    ForEach(PreferredTerminal.allCases) { terminal in
                        Text(terminal.rawValue + (TerminalLauncher.isInstalled(terminal) ? "" : " (not installed)"))
                            .tag(terminal)
                    }
                }

                Picker("Open sessions in", selection: Binding(
                    get: { store.terminalOpenBehavior },
                    set: { store.terminalOpenBehavior = $0 }
                )) {
                    ForEach(TerminalOpenBehavior.allCases) { behavior in
                        Text(behavior.label + (store.preferredTerminal.supports(behavior) ? "" : " (unavailable)"))
                            .tag(behavior)
                    }
                }
                .disabled(store.preferredTerminal.supportedOpenBehaviors.isEmpty)
            } header: {
                SettingsSectionHeader("Opening Sessions")
            } footer: {
                SettingsFootnote(openBehaviorFootnote)
            }

            if store.preferredTerminal == .custom {
                Section {
                    LabeledContent("Application") {
                        Button("Choose Terminal Application…") {
                            chooseCustomTerminalApplication()
                        }
                    }
                    TextField("Executable", text: $store.customTerminalExecutable)
                    TextField("Argument template", text: $store.customTerminalTemplate)
                } header: {
                    SettingsSectionHeader("Custom Terminal")
                } footer: {
                    SettingsFootnote(
                        "Choose any .app not listed above, then adjust its launch arguments if needed. Required placeholders: {latch} and {session}. Arguments are parsed directly and never passed to a shell."
                    )
                }
            }

            Section {
                SettingsFootnote(
                    "This is the default app Latch uses when opening a session. Closing Latch never stops sessions."
                )
            }
        }
        .formStyle(.grouped)
    }

    // MARK: - CLI

    private var cliTab: some View {
        Form {
            Section {
                TextField("Executable path", text: $store.latchExecutablePath)
                LabeledContent("Currently using") {
                    Text(store.activeCLIPath)
                        .textSelection(.enabled)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                HStack(spacing: 10) {
                    Button(store.isDiscoveringCLI ? "Searching…" : "Run `where latch`") {
                        Task { await store.discoverCLI() }
                    }
                    .disabled(store.isDiscoveringCLI)
                    Button("Diagnose") {
                        Task { await store.refreshCLIDiagnostics() }
                    }
                    Spacer()
                    Button("Use This CLI") { store.useConfiguredCLI() }
                        .disabled(!store.selectedCLIIsExecutable)
                }
            } header: {
                SettingsSectionHeader("Executable")
            } footer: {
                SettingsFootnote(
                    "Latch Desktop uses the independently installed CLI selected here. CLI updates replace both the command and its pinned tmux payload."
                )
            }

            discoverySection
            statusSection
            cliUpdatesSection
        }
        .formStyle(.grouped)
    }

    @ViewBuilder private var discoverySection: some View {
        if !store.discoveredLatchExecutables.isEmpty {
            Section {
                ForEach(store.discoveredLatchExecutables, id: \.self) { path in
                    Button {
                        store.selectCLI(path)
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "terminal")
                                .foregroundStyle(.secondary)
                            Text(path)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            Text(path == store.activeCLIPath ? "In use" : "Use")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(path == store.activeCLIPath)
                }
            } header: {
                SettingsSectionHeader("Found by `where latch`")
            }
        } else {
            Section {
                Text(store.cliInstallCommand)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                Button("Run Install Command in Terminal…") { store.runCLIInstaller() }
            } header: {
                SettingsSectionHeader("Install the CLI")
            } footer: {
                SettingsFootnote("No standalone Latch CLI was found on this Mac.")
            }
        }
    }

    @ViewBuilder private var statusSection: some View {
        if store.cliCapabilities != nil || store.cliDoctorReport != nil {
            Section {
                if let capabilities = store.cliCapabilities {
                    LabeledContent("CLI version", value: capabilities.productVersion)
                    LabeledContent("Protocol", value: String(capabilities.protocolVersion))
                    if !capabilities.capabilities.extensions.isEmpty {
                        LabeledContent(
                            "Extensions",
                            value: capabilities.capabilities.extensions.joined(separator: ", ")
                        )
                    }
                }
                if let doctor = store.cliDoctorReport {
                    if let tmuxVersion = doctor.tmuxVersion {
                        LabeledContent("Session kernel", value: tmuxVersion)
                    }
                    ForEach(doctor.findings) { finding in
                        Label(
                            finding.message,
                            systemImage: finding.severity == "error"
                                ? "exclamationmark.octagon"
                                : "exclamationmark.triangle"
                        )
                        .font(.caption)
                        .foregroundStyle(finding.severity == "error" ? Color.red : Color.secondary)
                    }
                }
            } header: {
                SettingsSectionHeader("Status")
            }
        }
    }

    private var cliUpdatesSection: some View {
        Section {
            HStack(spacing: 10) {
                Button(store.isCheckingCLIUpdate ? "Checking…" : "Check for CLI Update") {
                    Task { await store.checkForCLIUpdate() }
                }
                .disabled(store.isCheckingCLIUpdate || store.isUpdatingCLI)
                if store.cliUpdateReport?.status == .available,
                   store.cliCapabilities?.capabilities.selfUpdate == true {
                    Button(store.isUpdatingCLI ? "Updating…" : "Install CLI Update") {
                        Task { await store.updateCLI() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(store.isUpdatingCLI)
                }
                Spacer()
            }
            cliUpdateStatus
        } header: {
            SettingsSectionHeader("CLI Updates")
        }
    }

    // MARK: - App updates

    private var updatesTab: some View {
        Form {
            Section {
                Toggle("Check for updates automatically", isOn: $updates.automaticChecks)
                LabeledContent("Installed version") {
                    HStack(spacing: 12) {
                        Text(updates.installedVersionLabel)
                            .foregroundStyle(.secondary)
                        Button("Check Now") {
                            Task { await updates.check(userInitiated: true) }
                        }
                    }
                }
            } header: {
                SettingsSectionHeader("Desktop App Updates")
            } footer: {
                SettingsFootnote(
                    "Latch installs updates from its signed GitHub releases and verifies that each one is signed by the same developer before replacing itself."
                )
            }
        }
        .formStyle(.grouped)
    }

    private var openBehaviorFootnote: String {
        switch store.preferredTerminal {
        case .custom:
            return "Your argument template decides how a custom terminal opens, so this setting does not apply."
        case .ghostty:
            return "Ghostty cannot be told to open a tab from another app, so sessions always open in a new window."
        case .terminal:
            return "The Open button uses this; its menu can still override it per session. Opening a Terminal tab sends Command-T, so macOS asks Latch to control System Events once. If that is refused, Latch opens a new window instead."
        case .iTerm:
            return "The Open button uses this; its menu can still override it per session."
        }
    }

    @ViewBuilder private var cliUpdateStatus: some View {
        if let report = store.cliUpdateReport {
            switch report.status {
            case .current:
                SettingsFootnote("Latch CLI \(report.currentVersion) is current.")
            case .available:
                VStack(alignment: .leading, spacing: 8) {
                    Text("Latch CLI \(report.latestVersion ?? "update") is available; you have \(report.currentVersion).")
                    if let releaseURL = report.releaseURL {
                        Link("CLI release notes", destination: releaseURL)
                    }
                    if store.cliCapabilities?.capabilities.selfUpdate != true {
                        Text("This CLI cannot replace itself. Use its package manager or run:")
                        Text(store.cliInstallCommand)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                        Button("Run Installer in Terminal…") { store.runCLIInstaller() }
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            case .installed:
                SettingsFootnote(
                    "Updated the CLI from \(report.currentVersion) to \(report.latestVersion ?? "the latest release")."
                )
            }
        }
    }

    private func chooseCustomTerminalApplication() {
        let panel = NSOpenPanel()
        panel.title = "Choose Terminal Application"
        panel.message = "Select the application Latch should use to open session attachments."
        panel.prompt = "Choose"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.applicationBundle]

        guard panel.runModal() == .OK, let applicationURL = panel.url else { return }
        do {
            store.customTerminalExecutable = try TerminalLauncher.executablePath(forApplicationURL: applicationURL)
        } catch {
            store.errorMessage = error.localizedDescription
        }
    }
}

/// Section headers and footnotes are repeated across every settings tab, so they
/// live here to keep one typographic voice instead of ad-hoc `.font(.caption)`
/// calls scattered through the form.
struct SettingsSectionHeader: View {
    private let title: String

    init(_ title: String) { self.title = title }

    var body: some View {
        Text(title)
            .font(.headline)
            .padding(.bottom, 2)
    }
}

struct SettingsFootnote: View {
    private let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

private struct CLISetupView: View {
    @ObservedObject var store: SessionStore

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Connect the Latch CLI")
                .font(.title2)
                .fontWeight(.semibold)
            if store.isDiscoveringCLI {
                ProgressView("Running `where latch`…")
            } else if store.discoveredLatchExecutables.isEmpty {
                Text("Latch Desktop does not bundle the CLI, and `where latch` returned no paths. Run this verified-release installer in Terminal:")
                Text(store.cliInstallCommand)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(10)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                HStack {
                    Button("Run in Terminal…") { store.runCLIInstaller() }
                        .buttonStyle(.borderedProminent)
                    Button("Search Again") {
                        Task { await store.discoverCLI() }
                    }
                }
            } else {
                Text("`where latch` found the following executables. Choose the one Latch Desktop should use:")
                ForEach(store.discoveredLatchExecutables, id: \.self) { path in
                    Button {
                        store.selectCLI(path)
                    } label: {
                        HStack {
                            Image(systemName: "terminal")
                            Text(path).textSelection(.enabled)
                            Spacer()
                            Text("Use").foregroundStyle(.secondary)
                        }
                    }
                    .buttonStyle(.bordered)
                }
            }
            if !store.discoveredLatchExecutables.isEmpty {
                Text("To install or replace an outdated CLI, run:")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(store.cliInstallCommand)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                Button("Run Installer in Terminal…") { store.runCLIInstaller() }
            }
            HStack {
                Spacer()
                Button("Not Now") { store.shouldPresentCLISetup = false }
            }
        }
        .padding(24)
        .frame(width: 540)
    }
}
