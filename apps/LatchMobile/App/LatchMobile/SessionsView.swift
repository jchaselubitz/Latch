import LatchMobileKit
import SwiftUI

/// The sessions tab: what is running on the linked computer.
struct SessionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(PairingModel.self) private var pairing

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
                }
            }
            .navigationTitle("Sessions")
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
            .refreshable { await model.refreshSessions() }
        } else {
            List(model.sessions) { session in
                let route = model.route(for: session)
                NavigationLink {
                    destination(for: session, route: route)
                } label: {
                    SessionRow(session: session, route: route)
                }
            }
            .refreshable { await model.refreshSessions() }
            .overlay(alignment: .top) {
                if let error = model.sessionsError {
                    BannerView(text: error)
                }
            }
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
                            It's paired to observe. Open Latch on your Mac, find this phone \
                            under Remote Access, and raise it to Control.
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

            if let idle = session.idleMs {
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
            return "Open Settings to point this app at a `latch serve` gateway."
        }
        return """
        Looking for \(pairedMac) on this network. Keep the Mac's Remote Access enabled, or \
        add a `latch serve` gateway in Settings instead.
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
