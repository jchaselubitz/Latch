import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionsView: View {
    @ObservedObject var store: SessionStore
    @ObservedObject var updates: UpdateController
    @State private var showingCreate = false
    @State private var showingPrune = false
    @State private var pendingStop: PendingStopRequest?

    var body: some View {
        NavigationSplitView {
            List(store.filteredSessions, selection: $store.selection) { session in
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
            // Fixed footer: bulk actions live here instead of the toolbar so they stay
            // reachable regardless of list length or sidebar width.
            .safeAreaInset(edge: .bottom, spacing: 0) {
                SidebarFooter(
                    canStopSelected: store.hasSelectedLiveSessions,
                    stopSelected: {
                        let ids = store.selectedLiveSessionIDs
                        guard !ids.isEmpty else { return }
                        pendingStop = PendingStopRequest(sessionIDs: ids)
                    },
                    prune: {
                        Task {
                            await store.previewPrune()
                            showingPrune = store.prunePreview != nil
                        }
                    }
                )
            }
        } detail: {
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
                    systemImage: "rectangle.stack",
                    message: "Choose a session to inspect and manage it."
                )
            }
        }
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    Task { await store.refresh(showsProgress: true) }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(store.isRefreshing)
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
            Task {
                await store.previewPrune()
                showingPrune = store.prunePreview != nil
            }
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
