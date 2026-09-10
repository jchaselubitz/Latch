import LatchMobileKit
import SwiftUI

/// The sessions tab: what is running on the linked computer.
struct SessionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing
    @State private var creatingSession = false
    @State private var explainingGrant = false
    /// The session a Stop tap is asking about. Ending someone's work is not
    /// undoable, so it is always confirmed by name first.
    @State private var stopping: SessionSummary?
    /// Set when Stop was tapped on a Mac that serves the route but has not
    /// granted this phone control, so the reason can be said out loud.
    @State private var explainingStopGrant = false

    var body: some View {
        NavigationStack {
            Group {
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
            .navigationTitle("Sessions")
            .toolbar { newSessionButton }
            .sheet(isPresented: $creatingSession) {
                FolderPickerView(mode: .create)
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
        }
        .task { await refreshPermission() }
    }

    /// Shown whenever the Mac serves both new-session routes, and disabled
    /// when this phone's grant does not reach them — a control that explains
    /// itself is more use than one that quietly disappears.
    @ToolbarContentBuilder
    private var newSessionButton: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            if model.advertisesNewSessionCreation {
                Button {
                    if model.canCreateNewSession {
                        creatingSession = true
                    } else {
                        explainingGrant = true
                    }
                } label: {
                    Label("New session", systemImage: "plus")
                }
                .accessibilityLabel("New session")
                .accessibilityHint(
                    model.canCreateNewSession
                        ? "Choose a folder on your Mac and start a shell there"
                        : "Unavailable until this phone has control of your Mac"
                )
                // Left tappable when the grant is missing so the alert can say
                // what to change on the Mac.
                .opacity(model.canCreateNewSession ? 1 : 0.4)
            }
        }
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
            ScrollViewReader { proxy in
                List(model.sessions) { session in
                    let route = model.route(for: session)
                    NavigationLink {
                        destination(for: session, route: route)
                    } label: {
                        SessionRow(
                            session: session,
                            route: route,
                            isHighlighted: session.id == model.highlightedSessionID,
                            isStopping: model.stoppingSessionIDs.contains(session.id)
                        )
                    }
                    .id(session.id)
                    .listRowBackground(
                        session.id == model.highlightedSessionID
                            ? Color.accentColor.opacity(0.15)
                            : nil
                    )
                    // Both affordances, deliberately: the swipe is the fast
                    // path people already expect from a list, and the long
                    // press is the one that is discoverable without knowing
                    // the swipe is there.
                    .swipeActions(edge: .trailing) { stopButton(for: session) }
                    .contextMenu { stopButton(for: session) }
                }
                // A created session is pointed at, not opened. Attaching would
                // take the surface from the Mac, and creation never asked for
                // that.
                .onChange(of: model.highlightedSessionID) { _, created in
                    guard let created else { return }
                    withAnimation { proxy.scrollTo(created, anchor: .top) }
                    Task {
                        try? await Task.sleep(for: .seconds(4))
                        model.clearNewSessionHighlight()
                    }
                }
            }
            .refreshable {
                await refreshPermission()
                await model.refreshSessions()
            }
            .overlay(alignment: .top) {
                if model.sessionsStale {
                    BannerView(text: Self.staleBanner(model.linkState))
                } else if let error = model.sessionsError {
                    BannerView(text: error)
                }
            }
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

    static func staleBanner(_ state: AppModel.LinkState) -> String {
        switch state {
        case .macOffline: return "Your Mac is not reachable through the relay. This list is from before it went away."
        case .interrupted(.suspended, _): return "Reconnecting…"
        case .interrupted(.backoff(_, _, _), _): return "Connection lost. Showing the last known sessions."
        default: return "Reconnecting. Showing the last known sessions."
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
    /// Shown as a trailing glyph. On a build where the tap can be destructive,
    /// telling the user where it goes is not decoration.
    let route: SessionRoute
    /// True for the session this phone just created, briefly after creation.
    var isHighlighted = false
    /// True while this phone is waiting for the Mac to stop this session. The
    /// Mac waits out its own grace period first, so the wait is long enough
    /// that a row saying nothing would read as a tap that did nothing.
    var isStopping = false

    var body: some View {
        HStack(spacing: 12) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityLabel(session.state)

            VStack(alignment: .leading, spacing: 3) {
                Text(session.displayName)
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                HStack(spacing: 6) {
                    Text(session.directoryName)
                    Text("·")
                    Text(session.commandLabel)
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }

            Spacer(minLength: 8)

            if isStopping {
                Text("Stopping…")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else if let idle = session.idleMs {
                Text(Self.idleLabel(milliseconds: idle))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .monospacedDigit()
            }

            Image(systemName: destinationGlyph)
                .font(.caption)
                .foregroundStyle(.tertiary)
                .accessibilityLabel(destinationLabel)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isHighlighted ? [.isSelected] : [])
    }

    private var destinationGlyph: String {
        switch route {
        case .terminal: "terminal"
        case .chat: "bubble.left.and.bubble.right"
        case .unavailable: "exclamationmark.circle"
        }
    }

    private var destinationLabel: String {
        switch route {
        // Named separately because the two taps do different things to the
        // Mac, and the row is the last place to say so before one of them does.
        case .terminal(let autoAttach):
            autoAttach ? "Opens the terminal, taking it from your Mac" : "Opens the terminal"
        case .chat: "Opens the conversation"
        case .unavailable: "Cannot be opened"
        }
    }

    private var color: Color {
        switch session.state {
        case "running": return .green
        case "creating": return .yellow
        case "stopping": return .orange
        case "exited": return .secondary
        default: return .red
        }
    }

    /// Idle time, at the precision a person actually reads at a glance.
    static func idleLabel(milliseconds: Int) -> String {
        let seconds = milliseconds / 1000
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m" }
        if seconds < 86_400 { return "\(seconds / 3600)h" }
        return "\(seconds / 86_400)d"
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
            return "Open Settings and pair this phone with your Mac."
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
