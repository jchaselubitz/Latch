import LatchMobileKit
import SwiftUI

/// The sessions tab: what is running on the linked computer.
struct SessionsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationStack {
            Group {
                switch model.linkState {
                case .unlinked:
                    UnlinkedView()
                case .connecting:
                    ProgressView("Connecting…")
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
                NavigationLink {
                    ChatView(session: session)
                } label: {
                    SessionRow(session: session)
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
}

private struct SessionRow: View {
    let session: SessionSummary

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
        }
        .padding(.vertical, 2)
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
    var body: some View {
        MessageView(
            icon: "laptopcomputer.and.iphone",
            title: "No computer linked",
            detail: "Open Settings to point this app at a `latch serve` gateway."
        )
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
