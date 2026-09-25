import LatchMobileKit
import SwiftUI

/// The root screen: what is running on the linked computer, with the new
/// session pill and the Settings gear floating at the bottom.
///
/// A wide window keeps that list beside the open session. A narrow one
/// pushes the session over the list, which is the phone and a slim iPad pane.
struct SessionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @State private var creatingSession = false
    /// What the open picker starts: a shell, or an agent the Mac listed.
    @State private var creatingMode: FolderBrowserMode = .create
    @State private var explainingGrant = false
    /// The session a Stop tap is asking about. Ending someone's work is not
    /// undoable, so it is always confirmed by name first.
    @State private var stopping: SessionSummary?
    /// Set when Stop was tapped on a Mac that serves the route but has not
    /// granted this phone control, so the reason can be said out loud.
    @State private var explainingStopGrant = false
    /// The session open on this screen. A narrow window pushes it over the
    /// list; a wide one shows it in the second column. A `latch://` link
    /// clears it first so the linked session replaces whatever was open.
    @State private var selectedSessionID: String?
    /// The row each pushed session was opened from, for the moment the list
    /// refreshes without it: the open screen keeps its session.
    @State private var pushedSessions: [String: SessionSummary] = [:]
    @State private var showingSettings = false
    /// What the search field filters the list by: title, folder, or agent.
    @State private var searchQuery = ""

    var body: some View {
        Group {
            if browserLayout == .columns {
                columnBrowser
            } else {
                stackBrowser
            }
        }
        .onChange(of: model.requestedSessionID, initial: true) { _, _ in openLinkedSession() }
        .onChange(of: model.sessions) { _, _ in
            rememberSelectedSession()
            openLinkedSession()
        }
        .onChange(of: selectedSessionID) { _, _ in rememberSelectedSession() }
        .alert(
            "Can't open that session",
            isPresented: Binding(
                get: { model.requestedSessionError != nil },
                set: { if !$0 { model.clearRequestedSessionError() } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.requestedSessionError ?? "")
        }
        .sheet(isPresented: $creatingSession) {
            FolderPickerView(mode: creatingMode)
        }
        .sheet(isPresented: $showingSettings) {
            SettingsView()
        }
        .alert(
            "This phone can't start a session",
            isPresented: $explainingGrant
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.newSessionUnavailableExplanation ?? "")
        }
        .alert(
            "This phone can't stop a session",
            isPresented: $explainingStopGrant
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.sessionStopUnavailableExplanation ?? "")
        }
        // A stop ends whatever the session was doing on the Mac, so the
        // name is repeated back before anything is sent.
        .confirmationDialog(
            stopping.map { "Stop \($0.displayName)?" } ?? "Stop this session?",
            isPresented: Binding(
                get: { stopping != nil },
                set: { if !$0 { stopping = nil } }
            ),
            titleVisibility: .visible,
            presenting: stopping
        ) { session in
            Button("Stop session", role: .destructive) {
                stopping = nil
                Task { await model.stopSession(session) }
            }
            Button("Cancel", role: .cancel) { stopping = nil }
        } message: { session in
            Text(
                """
                Whatever is running in \(session.directoryName) on your Mac ends. The session \
                itself stays in this list so you can still read what it left behind.
                """
            )
        }
        .task { await refreshPermission() }
        .onOpenURL { url in
            // A `latch://sessions/<id>` link, usually from Overlord. Whatever
            // is on screen goes first, so the session opens over the list and
            // not under a sheet or on top of another session.
            guard let sessionID = SessionDeepLink.sessionID(from: url) else { return }
            showingSettings = false
            creatingSession = false
            stopping = nil
            selectedSessionID = nil
            Task { await model.requestSession(id: sessionID) }
        }
    }

    /// Wide and listing sessions: the list keeps its column and the open
    /// session fills the other. Anything else uses the stack below.
    private var browserLayout: SessionBrowserLayout {
        SessionBrowserLayoutPolicy.layout(
            isRegularWidth: horizontalSizeClass == .regular,
            showsSessionList: showsSessionList
        )
    }

    /// The cases in `linkContent` that draw the list. The other states fill
    /// the window on their own, so a second column would only sit empty.
    private var showsSessionList: Bool {
        switch model.linkState {
        case .linked:
            true
        case .interrupted(_, let capabilities):
            capabilities != nil && !model.sessions.isEmpty
        case .macOffline(let capabilities):
            capabilities != nil && !model.sessions.isEmpty
        default:
            false
        }
    }

    /// The phone, and a narrow iPad pane: the list is the root, and a session
    /// is pushed over it.
    private var stackBrowser: some View {
        NavigationStack(path: stackPath) {
            framedList(navigationBar: Self.navigationBarVisibility) {
                linkContent
            }
            .navigationDestination(for: SessionPush.self) { push in
                // The live row when the list still has it, so the screen
                // follows the session exactly as a row-built one did.
                if let session = resolvedSession(id: push.sessionID) {
                    destination(for: session, route: model.route(for: session))
                }
            }
        }
    }

    /// iPad at regular width: sessions on the left, the open session on the right.
    private var columnBrowser: some View {
        NavigationSplitView(columnVisibility: .constant(.doubleColumn)) {
            framedList(navigationBar: Self.navigationBarVisibility) {
                linkContent
            }
            .navigationSplitViewColumnWidth(min: 260, ideal: 320, max: 400)
            .accessibilityIdentifier("sessions.sidebar")
        } detail: {
            detailColumn
                .accessibilityIdentifier("sessions.detail")
        }
        .navigationSplitViewStyle(.balanced)
    }

    /// The open session, or the prompt to choose one. A fresh stack per
    /// session drops a terminal pushed over the previous conversation
    /// instead of carrying it across.
    private var detailColumn: some View {
        NavigationStack {
            if let session = selectedSession {
                destination(for: session, route: model.route(for: session))
                    .navigationBarBackButtonHidden(true)
            } else {
                ContentUnavailableView {
                    Label(
                        model.sessions.isEmpty ? "No session open" : "Select a session",
                        systemImage: "rectangle.stack"
                    )
                } description: {
                    Text(
                        model.sessions.isEmpty
                            ? "Sessions on your Mac are listed in the other column."
                            : "Choose a session and it opens here."
                    )
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color(.systemBackground))
                .toolbar(.hidden, for: .navigationBar)
                .accessibilityIdentifier("sessions.detail.empty")
            }
        }
        .id(selectedSessionID ?? "none")
        .environment(\.sessionBrowserLayout, .columns)
    }

    /// One session in the stack, or none. The column browser reads
    /// `selectedSessionID` directly and never mounts this path.
    private var stackPath: Binding<[SessionPush]> {
        Binding(
            get: {
                guard let selectedSessionID else { return [] }
                return [SessionPush(sessionID: selectedSessionID)]
            },
            set: { path in
                let next = path.last?.sessionID
                if next != selectedSessionID {
                    selectedSessionID = next
                }
            }
        )
    }

    private var selectedSession: SessionSummary? {
        guard let selectedSessionID else { return nil }
        return resolvedSession(id: selectedSessionID)
    }

    /// The live row when the list still has it, otherwise the copy kept when
    /// the session was opened.
    private func resolvedSession(id: String) -> SessionSummary? {
        model.sessions.first { $0.id == id } ?? pushedSessions[id]
    }

    /// The title is drawn by the list itself, up where the stock large title
    /// leaves a band of empty screen. The bar keeps only the back label of a
    /// pushed screen, and where the search field no longer lives in it, it
    /// goes altogether. The sidebar uses that same bar.
    private func framedList<Content: View>(
        navigationBar: Visibility,
        @ViewBuilder content: () -> Content
    ) -> some View {
        content()
            .navigationTitle("Latch")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Color.clear.frame(width: 1, height: 1).accessibilityHidden(true)
                }
            }
            .toolbar(navigationBar, for: .navigationBar)
            // Every state, the unpaired ones included: the gear is the only
            // way to reach pairing now that there is no tab bar.
            .safeAreaInset(edge: .bottom) { floatingActions }
    }

    @ViewBuilder
    private var linkContent: some View {
        switch model.linkState {
        case .unlinked:
            UnlinkedView(pairedMac: pairedMacName)
        case .connecting:
            ProgressView("Connecting…")
        case .incompatible(let mismatch):
            MessageView(
                icon: mismatch.icon,
                title: mismatch.title,
                detail: mismatch.detail
            )
        case .failed(let reason):
            MessageView(
                icon: "exclamationmark.triangle",
                title: "Cannot reach that computer",
                detail: reason
            )
        case .linked:
            sessionList
        case .interrupted(let link, let capabilities):
            if capabilities != nil, !model.sessions.isEmpty {
                // Cached rows stay readable and are marked stale;
                // nothing is fetched until the owner reports ready.
                sessionList
            } else {
                MessageView(
                    icon: "arrow.triangle.2.circlepath",
                    title: Self.interruptedTitle(link),
                    detail: Self.interruptedDetail(link)
                )
            }
        case .macOffline(let capabilities) where capabilities != nil && !model.sessions.isEmpty:
            // Same bargain as an interrupted link: the last known rows
            // stay readable, and the status line says they are stale.
            sessionList
        case .macOffline:
            MessageView(
                icon: "laptopcomputer.slash",
                title: "Mac unavailable through the relay",
                detail: "This phone could not find your Mac on the relay. It may be asleep, disconnected, or have remote access turned off. This phone keeps checking; wake the Mac to continue."
            )
        case .revoked(let reason):
            MessageView(
                icon: "xmark.shield",
                title: "This phone was unpaired",
                detail: reason
            )
        case .pairingRequired(let reason):
            MessageView(
                icon: "qrcode",
                title: "Pair again",
                detail: reason
            )
        }
    }

    /// The pill that starts a session, and the gear that opens Settings.
    private var floatingActions: some View {
        HStack(alignment: .center) {
            if model.advertisesNewSessionCreation {
                newSessionPill
            }
            Spacer(minLength: 12)
            Button {
                showingSettings = true
            } label: {
                FloatingControl(systemImage: "gearshape", size: 48)
                    .foregroundStyle(.primary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Settings")
            .accessibilityIdentifier("sessions.settings")
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 8)
    }

    /// Shown whenever the Mac serves both new-session routes, and dimmed
    /// when this phone's grant does not reach them — a control that explains
    /// itself is more use than one that quietly disappears.
    ///
    /// A Mac that lists agents turns the pill into a menu: a shell, or each
    /// agent it will launch directly. A Mac that lists none keeps a one-tap
    /// shell button.
    private var newSessionPill: some View {
        let agents = model.availableSessionAgents
        return Group {
            if agents.isEmpty {
                Button {
                    startCreating(.create)
                } label: {
                    newSessionLabel
                }
            } else {
                Menu {
                    Button {
                        startCreating(.create)
                    } label: {
                        Label("Shell", systemImage: "terminal")
                    }
                    ForEach(agents, id: \.self) { agent in
                        Button {
                            startCreating(.createAgent(agent))
                        } label: {
                            Label(agent.displayName, systemImage: "sparkles")
                        }
                    }
                } label: {
                    newSessionLabel
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("New session")
        .accessibilityIdentifier("sessions.newSession")
        .accessibilityHint(
            model.canCreateNewSession
                ? agents.isEmpty
                    ? "Choose a folder on your Mac and start a shell there"
                    : "Choose a shell or an agent, then a folder on your Mac to start it in"
                : "Unavailable until this phone has control of your Mac"
        )
        // Left tappable when the grant is missing so the alert can say what
        // to change on the Mac.
        .opacity(model.canCreateNewSession ? 1 : 0.45)
    }

    private var newSessionLabel: some View {
        FloatingControl(shape: .capsule, size: 48, tint: .accentColor) {
            HStack(spacing: 6) {
                Image(systemName: "plus")
                    .fontWeight(.semibold)
                Text("Session")
                    .lineLimit(1)
            }
            .font(.body.weight(.semibold))
            .padding(.horizontal, 4)
        }
        .foregroundStyle(.white)
    }

    private func startCreating(_ mode: FolderBrowserMode) {
        if model.canCreateNewSession(agent: mode.agent) {
            creatingMode = mode
            creatingSession = true
        } else {
            explainingGrant = true
        }
    }

    /// Opens the linked session once the list holds it. Nothing is attached
    /// by the link itself: the destination is the one `route(for:)` picks, so
    /// a terminal still asks for Face ID and a grant still gates it.
    private func openLinkedSession() {
        guard let session = model.takeRequestedSession() else { return }
        select(session)
    }

    private func select(_ session: SessionSummary) {
        pushedSessions[session.id] = session
        selectedSessionID = session.id
    }

    /// Keeps the last known row for the open session, and only that one.
    private func rememberSelectedSession() {
        guard let selectedSessionID else {
            pushedSessions = [:]
            return
        }
        if let session = model.sessions.first(where: { $0.id == selectedSessionID }) {
            pushedSessions = [selectedSessionID: session]
        } else {
            pushedSessions = pushedSessions.filter { $0.key == selectedSessionID }
        }
    }

    /// Hidden from iOS 26, where the search field sits in the bottom bar and
    /// an inline bar would only hold the list's title away from the top of
    /// the screen. Earlier systems draw the search field in that bar, so
    /// hiding it there would take search with it.
    private static var navigationBarVisibility: Visibility {
        if #available(iOS 26, *) { .hidden } else { .visible }
    }

    /// In a sidebar the search field stays open, because a pull-to-reveal
    /// field is easy to miss beside the open session. iOS 26 draws search
    /// in the bottom bar on its own, so the automatic placement stays.
    private var searchPlacement: SearchFieldPlacement {
        if browserLayout == .columns, Self.navigationBarVisibility == .visible {
            return .navigationBarDrawer(displayMode: .always)
        }
        return .automatic
    }

    /// The paired Mac's name, when this phone has finished pairing.
    private var pairedMacName: String? {
        guard case .paired(let record) = pairing.state else { return nil }
        return record.mac.displayName
    }

    @ViewBuilder
    private var sessionList: some View {
        if model.sessions.isEmpty {
            MessageView(
                icon: "moon.zzz",
                title: "No sessions",
                detail: model.sessionsError
                    ?? "Start one on your computer with `latch new`, then pull to refresh."
            )
            .refreshable {
                await refreshPermission()
                await model.refreshSessions()
            }
        } else {
            let groups = SessionListGroup.grouped(model.sessions, matching: searchQuery)
            ScrollViewReader { proxy in
                List {
                    SessionListHeader(
                        status: SessionListLinkStatus(model.linkState),
                        deviceName: pairedMacName
                    )
                    .listRowSeparator(.hidden)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets(top: 8, leading: 16, bottom: 4, trailing: 16))
                    ForEach(groups) { group in
                        Section(group.section.title) {
                            ForEach(group.sessions) { session in
                                row(for: session)
                            }
                        }
                    }
                }
                .listStyle(.plain)
                .overlay {
                    if groups.isEmpty {
                        ContentUnavailableView.search(text: searchQuery)
                    }
                }
                // A created session is pointed at, not opened. Attaching would
                // take the surface from the Mac, and creation never asked for
                // that. A search that would hide it is cleared first.
                .onChange(of: model.highlightedSessionID) { _, created in
                    guard let created else { return }
                    searchQuery = ""
                    if reduceMotion {
                        proxy.scrollTo(created, anchor: .top)
                    } else {
                        withAnimation { proxy.scrollTo(created, anchor: .top) }
                    }
                    Task {
                        try? await Task.sleep(for: .seconds(4))
                        model.clearNewSessionHighlight()
                    }
                }
            }
            .searchable(text: $searchQuery, placement: searchPlacement, prompt: "Search sessions")
            .refreshable {
                await refreshPermission()
                await model.refreshSessions()
            }
            .overlay(alignment: .bottom) {
                // Link trouble is the status line's job; this strip is left
                // for a list request that failed on a working link.
                if !model.sessionsStale, let error = model.sessionsError {
                    BannerView(text: error)
                }
            }
        }
    }

    private func row(for session: SessionSummary) -> some View {
        let route = model.route(for: session)
        let isHighlighted = session.id == model.highlightedSessionID
        let isCurrent = browserLayout == .columns && session.id == selectedSessionID
        let label = SessionRow(
            session: session,
            route: route,
            isHighlighted: isHighlighted,
            isStopping: model.stoppingSessionIDs.contains(session.id),
            isCurrent: isCurrent
        )
        return Group {
            if browserLayout == .columns {
                // Selecting fills the other column. A link here would push a
                // second copy of the session inside the sidebar.
                Button {
                    select(session)
                } label: {
                    label
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            } else {
                NavigationLink(value: SessionPush(sessionID: session.id)) {
                    label
                }
            }
        }
        .id(session.id)
        .listRowSeparator(.hidden)
        .listRowBackground(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(rowFill(isHighlighted: isHighlighted, isCurrent: isCurrent))
                .padding(.horizontal, 8)
        )
        // Both affordances, deliberately: the swipe is the fast path people
        // already expect from a list, and the long press is the one that is
        // discoverable without knowing the swipe is there. The long press
        // also carries what the single-line row no longer says.
        .swipeActions(edge: .trailing) { stopButton(for: session) }
        .contextMenu {
            Section {
                Label(session.cwd, systemImage: "folder")
                Label(session.commandLabel, systemImage: "chevron.left.forwardslash.chevron.right")
                Label(session.connector.detailLabel, systemImage: "sparkles")
            }
            stopButton(for: session)
        }
    }

    /// The Stop control for one row, or nothing at all.
    ///
    /// Shown whenever the Mac serves the route and the session is still live,
    /// and left tappable without the grant so the alert can say what to change
    /// on the Mac — the same bargain the New session button makes.
    @ViewBuilder
    private func stopButton(for session: SessionSummary) -> some View {
        if model.advertisesSessionStop, session.isRunning {
            Button(role: .destructive) {
                if model.canStopSessions {
                    stopping = session
                } else {
                    explainingStopGrant = true
                }
            } label: {
                Label(
                    model.stoppingSessionIDs.contains(session.id) ? "Stopping…" : "Stop",
                    systemImage: "stop.circle"
                )
            }
            .disabled(model.stoppingSessionIDs.contains(session.id))
            .accessibilityHint(
                model.canStopSessions
                    ? "Ends what is running in this session on your Mac"
                    : "Unavailable until this phone has control of your Mac"
            )
        }
    }

    private func rowFill(isHighlighted: Bool, isCurrent: Bool) -> Color {
        if isCurrent { return Color.accentColor.opacity(0.18) }
        if isHighlighted { return Color.accentColor.opacity(0.15) }
        return .clear
    }

    static func interruptedTitle(_ link: RemoteLinkState) -> String {
        switch link {
        case .suspended: return "Reconnecting"
        case .connecting: return "Connecting"
        case .backoff: return "Connection lost"
        default: return "Reconnecting"
        }
    }

    static func interruptedDetail(_ link: RemoteLinkState) -> String {
        switch link {
        case .backoff(_, let nextRetryAt, let reason):
            let seconds = max(0, Int(nextRetryAt.timeIntervalSinceNow.rounded()))
            return "\(reason) Trying again in \(seconds)s."
        case .connecting(let attempt) where attempt > 0:
            return "Trying again (attempt \(attempt + 1))."
        default:
            return "Restoring the secure connection to your Mac."
        }
    }


    /// The screen a tap lands on. `AppModel.route(for:)` decides; this only
    /// builds what it named.
    @ViewBuilder
    private func destination(for session: SessionSummary, route: SessionRoute) -> some View {
        switch route {
        case .terminal(let autoAttach):
            TerminalView(session: session, autoAttach: autoAttach)
        case .chat:
            ChatView(session: session)
        case .chatUnavailable(let block):
            ChatUnavailableView(session: session, block: block)
        case .unavailable(let block):
            SessionUnavailableView(session: session, block: block)
        }
    }

    /// A grant can change while this tab remains on screen. Re-read it when
    /// the list appears and on pull-to-refresh, then update the linked model
    /// synchronously so the next route decision sees the new permission.
    private func refreshPermission() async {
        await pairing.refreshPermission()
        if !model.applyPairedDeviceRecord(pairing.record) {
            await model.connectPairedDevice(pairing.record)
        }
    }
}

/// One screen pushed over the list, named by session so the destination is
/// built from the live row.
private struct SessionPush: Hashable {
    let sessionID: String
}

/// A Chat choice never opens a terminal by implication. When Chat cannot open,
/// this screen says why and makes terminal takeover a separate, deliberate
/// action when the Mac and this phone permit it.
private struct ChatUnavailableView: View {
    let session: SessionSummary
    let block: ChatRouteBlock

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: "exclamationmark.bubble")
        } description: {
            Text(detail)
        } actions: {
            recovery
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var title: String {
        switch block {
        case .noConnector: "Chat isn't available for this session"
        case .noConversationEndpoint: "Chat needs a newer Mac service"
        }
    }

    private var detail: String {
        switch block {
        case .noConnector:
            "This session is a shell or was started without a recognized agent connector. Start a new Claude Code or Codex session to use Chat."
        case .noConversationEndpoint:
            "This Mac's Latch service does not offer the Conversation Hub. Update Latch on the Mac, then reopen this session."
        }
    }

    @ViewBuilder
    private var recovery: some View {
        switch terminalRecovery {
        case .available:
            NavigationLink("Take terminal") {
                // Reaching this screen began with a Chat tap. The terminal is
                // only taken after this second, explicit tap.
                TerminalView(session: session, autoAttach: false)
                    .pushedOverSessionColumn()
            }
            .buttonStyle(.borderedProminent)
        case .needsControlGrant:
            Text("To take the terminal instead, set this phone to Control and enable Allow terminal in Latch on your Mac.")
                .multilineTextAlignment(.center)
        case .unavailable:
            Text("Use `latch attach` on the Mac for this session.")
        }
    }

    private var terminalRecovery: TerminalRecovery {
        switch block {
        case .noConnector(let recovery), .noConversationEndpoint(let recovery): recovery
        }
    }
}

/// Why neither screen can be opened, said as what to do rather than what
/// failed.
private struct SessionUnavailableView: View {
    let session: SessionSummary
    let block: SessionRouteBlock

    var body: some View {
        Group {
            switch block {
            case .needsControlGrant:
                // The preview needs only `observe`, so an observing phone may
                // read the pane. Showing it behind the explanation is the
                // difference between an explanation and a dead end.
                VStack(spacing: 0) {
                    TerminalStillView(session: session)
                    VStack(spacing: 8) {
                        Text("This phone can't open a terminal")
                            .font(.headline)
                        Text(
                            """
                            This phone does not currently have terminal access. Open Latch on your \
                            Mac, find this phone under Remote Access, set it to Control, and turn on \
                            Allow terminal.
                            """
                        )
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(16)
                    .background(.bar)
                }
            case .noTerminalEndpoint:
                MessageView(
                    icon: "arrow.up.circle",
                    title: "This Mac has no terminal route",
                    detail: """
                    This session has no conversation connector, and the Mac is older than the \
                    terminal route that would stand in for one. Update Latch on the Mac.
                    """
                )
            case .noConversation:
                MessageView(
                    icon: "terminal",
                    title: "Nothing to open",
                    detail: """
                    This Mac offers neither the Conversation Hub nor a terminal route. Use \
                    `latch attach` on the Mac for this session.
                    """
                )
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }
}

private struct SessionRow: View {
    let session: SessionSummary
    /// Not drawn: the section and the row say enough to choose by. It is
    /// still spoken, because the two taps do different things to the Mac.
    let route: SessionRoute
    /// True for the session this phone just created, briefly after creation.
    var isHighlighted = false
    /// True while this phone is waiting for the Mac to stop this session. The
    /// Mac waits out its own grace period first, so the wait is long enough
    /// that a row saying nothing would read as a tap that did nothing.
    var isStopping = false
    /// True when this row's session is the one open in the other column.
    var isCurrent = false

    var body: some View {
        HStack(spacing: 8) {
            if session.isTransitioning {
                Circle()
                    .fill(.orange)
                    .frame(width: 7, height: 7)
                    .accessibilityHidden(true)
            }

            let headline = session.headline
            VStack(alignment: .leading, spacing: 2) {
                Text(headline.primary)
                    .font(.body)
                    .lineLimit(2)
                if let subtitle {
                    subtitle
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .layoutPriority(1)

            Spacer(minLength: 8)

            if isStopping {
                Text("Stopping…")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(accessibilityState)
        .accessibilityHint(destinationLabel)
        .accessibilityAddTraits(isHighlighted || isCurrent ? [.isSelected] : [])
    }

    /// The line under the description: handle, agent, then idle time, joined
    /// by dots in one run of text so a narrow row shortens the tail rather
    /// than wrapping. Nil when the session has nothing to say beneath it.
    private var subtitle: Text? {
        let fields = session.subtitleFields(showingIdle: !isStopping)
        guard let first = fields.first else { return nil }
        return fields.dropFirst().reduce(subtitleText(first)) { line, field in
            line + Text(" · ") + subtitleText(field)
        }
    }

    private func subtitleText(_ field: SessionSubtitleField) -> Text {
        switch field {
        case .shellFolder(let folder): Text("\(Image(systemName: "keyboard")) \(folder)")
        case .idle(let idle): Text(idle).monospacedDigit()
        case .handle(let text), .agent(let text): Text(text)
        }
    }

    private var accessibilityState: String {
        if isStopping { return "Stopping" }
        return session.connector == .none ? "Shell, \(session.state)" : session.state
    }

    private var destinationLabel: String {
        switch route {
        // Named separately because the two taps do different things to the
        // Mac, and the row is the last place to say so before one of them does.
        case .terminal(let autoAttach):
            autoAttach ? "Opens the terminal, taking it from your Mac" : "Opens the terminal"
        case .chat: "Opens the conversation"
        case .chatUnavailable: "Chat is unavailable; opens recovery options"
        case .unavailable: "Cannot be opened"
        }
    }
}

/// The list's own title row: "Latch" on the left, and opposite it the pill
/// that says whether the rows below are live and which Mac they come from.
private struct SessionListHeader: View {
    let status: SessionListLinkStatus?
    let deviceName: String?

    var body: some View {
        HStack(alignment: .center) {
            Text("Latch")
                .font(.largeTitle.weight(.bold))
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 12)
            if let status {
                LinkStatusPill(status: status, deviceName: deviceName)
            }
        }
    }
}

/// An outlined pill: a colored dot for the link state and, when the link is
/// live, the name of the Mac on the other end of it.
private struct LinkStatusPill: View {
    let status: SessionListLinkStatus
    let deviceName: String?

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
            Text(status.pillText(deviceName: deviceName))
                .lineLimit(1)
        }
        .font(.footnote.weight(.medium))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .overlay(
            Capsule(style: .continuous)
                .strokeBorder(.secondary.opacity(0.35), lineWidth: 1)
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityIdentifier("sessions.linkStatus")
    }

    private var accessibilityLabel: String {
        var parts = [status.label]
        if status == .connected, let deviceName, !deviceName.isEmpty { parts.append("to \(deviceName)") }
        if let detail = status.accessibilityDetail { parts.append(detail) }
        return parts.joined(separator: ". ")
    }

    private var color: Color {
        switch status {
        case .connected: .green
        case .reconnecting: .orange
        case .macUnavailable: .secondary
        }
    }
}

private struct UnlinkedView: View {
    /// The Mac this phone paired with, when there is one.
    let pairedMac: String?

    var body: some View {
        MessageView(
            icon: pairedMac == nil ? "laptopcomputer.and.iphone" : "cable.connector",
            title: pairedMac == nil ? "No computer linked" : "Paired, but not linked",
            detail: detail
        )
    }

    private var detail: String {
        guard let pairedMac else {
            return "Tap the gear to open Settings and pair this phone with your Mac."
        }
        return """
        Looking for \(pairedMac) over authenticated Remote Link. Keep Remote Access enabled \
        on the Mac and check that the control plane is reachable.
        """
    }
}

/// A centered empty or error state.
struct MessageView: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        ScrollView {
            VStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
            .frame(maxWidth: .infinity)
            .padding(.top, 60)
        }
    }
}

/// A non-blocking error strip, for a failure that did not clear the screen.
struct BannerView: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.footnote)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.thinMaterial)
    }
}

