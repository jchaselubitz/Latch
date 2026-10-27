import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @ObservedObject var remoteAccess: RemoteAccessController
    let openSettings: () -> Void
    @State private var showingCreate = false
    @State private var showingPrune = false
    @State private var pendingStop: PendingStopRequest?
    @State private var searchFocusRequest = 0
    @State private var showingInspector = true

    var body: some View {
        NavigationSplitView {
            // The column width alone bounds the sidebar. Left to its content,
            // the sidebar's hosting view reports a new minimum size from inside
            // AppKit's constraint pass while the divider is dragged, and AppKit
            // aborts the app over the constraints it keeps dirtying.
            SessionsSidebar(store: store, remoteAccess: remoteAccess, prune: startPrune)
                .frame(minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity)
                .navigationSplitViewColumnWidth(min: 200, ideal: 224, max: 300)
        } detail: {
            sessionList
        }
        .sessionInspector(isPresented: $showingInspector) {
            inspectorContent
        }
        .onChange(of: store.selection) { selection in
            if !selection.isEmpty { showingInspector = true }
            Task { await store.loadDetails() }
        }
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
        .confirmationDialog(
            pendingStopTitle,
            isPresented: pendingStopBinding,
            titleVisibility: .visible
        ) {
            if pendingStop != nil {
                Button("Stop", role: .destructive) { confirmPendingStop(force: false) }
                Button("Force Stop", role: .destructive) { confirmPendingStop(force: true) }
            }
            Button("Cancel", role: .cancel) { pendingStop = nil }
        } message: {
            Text(pendingStopMessage)
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

    private var sessionList: some View {
        let sessions = store.filteredSessions
        let running = sessions.filter(\.state.isLive)
        let stopped = sessions.filter { !$0.state.isLive }
        return List(selection: $store.selection) {
            if !running.isEmpty {
                Section("Running") {
                    ForEach(running) { sessionRow($0) }
                }
            }
            if !stopped.isEmpty {
                Section("Stopped") {
                    ForEach(stopped) { sessionRow($0) }
                }
            }
        }
        .listStyle(.inset)
        .overlay {
            if store.sessions.isEmpty && !store.isRefreshing {
                EmptyStateView(
                    title: "No Latch Sessions",
                    systemImage: "rectangle.stack.badge.plus",
                    message: "Create a persistent shell session to get started."
                )
            } else if sessions.isEmpty && !store.sessions.isEmpty {
                EmptyStateView(
                    title: "No Matching Sessions",
                    systemImage: "line.3.horizontal.decrease.circle",
                    message: "Nothing in \(store.filter.title) matches the current search."
                )
            }
        }
        .navigationTitle(store.filter.title)
        .navigationSubtitle(listSubtitle)
        // No real minimum here: on the macOS 26 SDK any minimum the detail
        // column reports, whether a fixed one or the list's own, sends the
        // split view into an endless constraint pass when a neighbouring
        // column is resized, and AppKit aborts the app over it. Zero keeps
        // the reported size constant; the window's minimum still applies.
        .frame(minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(action: openSettings) {
                    Label("Settings", systemImage: "gearshape")
                }
                .help("Settings")
            }
            ToolbarItem(placement: .primaryAction) {
                SessionSearchField(text: $store.search, focusRequest: searchFocusRequest)
                    .frame(width: 200)
                    .background {
                        Button("Search Sessions") { searchFocusRequest += 1 }
                            .keyboardShortcut("f", modifiers: .command)
                            .hidden()
                            .accessibilityHidden(true)
                    }
            }
            ToolbarItem(placement: .primaryAction) {
                Button { showingCreate = true } label: {
                    Label("New Session", systemImage: "plus")
                }
                .keyboardShortcut("n")
                .disabled(!store.canCreateSessions)
                .help("New Session")
                .latchProminentToolbarButton()
            }
            ToolbarItem(placement: .primaryAction) {
                Button { showingInspector.toggle() } label: {
                    Label("Session Info", systemImage: "sidebar.right")
                }
                .help(showingInspector ? "Hide Session Info" : "Show Session Info")
                .accessibilityValue(showingInspector ? "Shown" : "Hidden")
            }
        }
    }

    private func sessionRow(_ session: SessionSummary) -> some View {
        SessionRow(session: session)
            .tag(session.id)
            .contextMenu {
                let canOpen = session.state.isAttachable && store.canAttachSessions
                let otherLiveIDs = store.otherLiveSessionIDs(keeping: session.id)
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
                Divider()
                Button("Copy Attach Command") {
                    Task { await store.copyAttachCommand(session.id) }
                }
                Divider()
                if session.state.isLive {
                    Button("Stop…", role: .destructive) {
                        pendingStop = stopTargets(for: session)
                    }
                }
                Button("Stop Other Sessions…", role: .destructive) {
                    pendingStop = PendingStopRequest(
                        sessionIDs: otherLiveIDs,
                        keepingID: session.id,
                        keepingName: session.name
                    )
                }
                .disabled(otherLiveIDs.isEmpty)
            }
    }

    private var listSubtitle: String {
        let running = store.filteredSessions.filter(\.state.isLive).count
        let total = store.filteredSessions.count
        if total == 0 { return "No sessions" }
        if running == total { return "\(running) running" }
        return "\(running) running · \(total - running) stopped"
    }

    @ViewBuilder private var inspectorContent: some View {
        if store.selection.count > 1 {
            MultiSessionSelectionView(store: store)
        } else if store.selection.count == 1,
                  let selectedID = store.selection.first,
                  let details = store.details,
                  details.id == selectedID {
            SessionDetailView(store: store, session: details)
        } else {
            EmptyStateView(
                title: "Select a Session",
                systemImage: "info.circle",
                message: "Choose a session to inspect and manage it."
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func startPrune() {
        Task {
            await store.previewPrune()
            showingPrune = store.prunePreview != nil
        }
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { store.errorMessage != nil },
            set: { if !$0 { store.errorMessage = nil } }
        )
    }

    private var pendingStopBinding: Binding<Bool> {
        Binding(
            get: { pendingStop != nil },
            set: { if !$0 { pendingStop = nil } }
        )
    }

    private var pendingStopTitle: String {
        guard let pendingStop else { return "Stop Session" }
        if pendingStop.keepingName != nil {
            let count = pendingStop.sessionIDs.count
            return count == 1 ? "Stop the other session?" : "Stop \(count) other sessions?"
        }
        if pendingStop.sessionIDs.count == 1,
           let name = store.sessions.first(where: { $0.id == pendingStop.sessionIDs[0] })?.name {
            return "Stop \(name)?"
        }
        return "Stop \(pendingStop.sessionIDs.count) sessions?"
    }

    private var pendingStopMessage: String {
        if let keepingName = pendingStop?.keepingName {
            return "Stopping ends every live session except \(keepingName). Each final screen is retained for later inspection."
        }
        if pendingStop?.sessionIDs.count == 1 {
            return "Stopping ends the child process but retains its final screen for later inspection."
        }
        return "Stopping ends each child process but retains its final screen for later inspection."
    }

    private func stopTargets(for session: SessionSummary) -> PendingStopRequest {
        let ids: [String]
        if store.selection.contains(session.id), store.selection.count > 1 {
            ids = store.selectedLiveSessionIDs
        } else {
            ids = session.state.isLive ? [session.id] : []
        }
        return PendingStopRequest(sessionIDs: ids)
    }

    private func confirmPendingStop(force: Bool) {
        guard let pendingStop else { return }
        let ids = pendingStop.sessionIDs
        let keepingID = pendingStop.keepingID
        self.pendingStop = nil
        Task {
            if let keepingID {
                await store.stopOthers(keeping: keepingID, force: force)
            } else {
                await store.stopSessions(ids, force: force)
            }
        }
    }

    private func handleMenuRequests() {
        if store.shouldPresentNewSession {
            store.shouldPresentNewSession = false
            showingCreate = true
        }
        if store.shouldPresentPrune {
            store.shouldPresentPrune = false
            startPrune()
        }
    }
}

/// Shows session details as a trailing inspector column on macOS 14 and later,
/// and as a fixed-width drawer on macOS 13, which has no inspector.
private struct SessionInspector<Inspector: View>: ViewModifier {
    @Binding var isPresented: Bool
    let inspector: () -> Inspector

    func body(content: Content) -> some View {
        if #available(macOS 14.0, *) {
            content.inspector(isPresented: $isPresented) {
                // A fixed minimum keeps the column from reporting a new size
                // every time the selection swaps the content underneath it.
                inspector()
                    .frame(minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity)
                    .inspectorColumnWidth(min: 300, ideal: 340, max: 420)
            }
        } else {
            HStack(spacing: 0) {
                content
                if isPresented {
                    Divider()
                    inspector()
                        .frame(width: 330)
                        .frame(maxHeight: .infinity)
                        .background(.regularMaterial)
                }
            }
        }
    }
}

private extension View {
    func sessionInspector<Inspector: View>(
        isPresented: Binding<Bool>,
        @ViewBuilder content: @escaping () -> Inspector
    ) -> some View {
        modifier(SessionInspector(isPresented: isPresented, inspector: content))
    }
}

/// A native search field with live filtering and Command-F focus support.
private struct SessionSearchField: NSViewRepresentable {
    @Binding var text: String
    let focusRequest: Int

    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }

    func makeNSView(context: Context) -> NSSearchField {
        let field = NSSearchField()
        field.placeholderString = "Search sessions"
        field.setAccessibilityLabel("Search sessions")
        field.sendsSearchStringImmediately = true
        field.sendsWholeSearchString = false
        field.delegate = context.coordinator
        return field
    }

    func updateNSView(_ field: NSSearchField, context: Context) {
        context.coordinator.text = $text
        if field.stringValue != text { field.stringValue = text }
        if context.coordinator.focusRequest != focusRequest {
            context.coordinator.focusRequest = focusRequest
            field.window?.makeFirstResponder(field)
        }
    }

    final class Coordinator: NSObject, NSSearchFieldDelegate {
        var text: Binding<String>
        var focusRequest = 0

        init(text: Binding<String>) { self.text = text }

        func controlTextDidChange(_ notification: Notification) {
            guard let field = notification.object as? NSSearchField else { return }
            text.wrappedValue = field.stringValue
        }
    }
}

struct PendingStopRequest: Identifiable {
    let id = UUID()
    let sessionIDs: [String]
    /// When set, confirmation is "stop others"; the named session is left running.
    let keepingID: String?
    let keepingName: String?

    init(sessionIDs: [String], keepingID: String? = nil, keepingName: String? = nil) {
        self.sessionIDs = sessionIDs
        self.keepingID = keepingID
        self.keepingName = keepingName
    }
}
