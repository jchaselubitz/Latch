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
    ///
    /// Pairing and the gateway link are separate arrangements in this build,
    /// so the empty state has to tell them apart: a phone that paired
    /// successfully and a phone that was never set up are not the same
    /// situation, and the same sentence for both makes the first look broken.
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
    /// The Mac this phone paired with, when there is one.
    let pairedMac: String?

    var body: some View {
        MessageView(
            icon: pairedMac == nil ? "laptopcomputer.and.iphone" : "cable.connector",
            title: pairedMac == nil ? "No computer linked" : "Paired, but not linked",
            detail: detail
        )
    }

    /// Pairing enrolls this phone's identity; it does not by itself open a
    /// path to the Mac. A paired phone reaching this screen has done nothing
    /// wrong, so it is told what is still missing rather than being handed the
    /// wording for a phone that never paired at all.
    private var detail: String {
        guard let pairedMac else {
            return "Open Settings to point this app at a `latch serve` gateway."
        }
        return """
        This phone is paired with \(pairedMac), which enrolled its identity. Sessions come \
        from a `latch serve` gateway, which is a separate step: add its address in Settings.
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
