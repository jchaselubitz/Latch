import LatchMobileKit
import SwiftUI

struct SessionRow: View {
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
            VStack(alignment: .leading, spacing: 3) {
                Text(headline.primary)
                    .font(.body)
                    .lineLimit(2)
                // The status leads the line beneath the title rather than
                // trailing beside it: there a long title squeezed the badge
                // against the edge until it wrapped. Here the title has the
                // full width, the badge keeps its shape, and the subtitle is
                // what gives way.
                if hasStatus || subtitle != nil {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        status
                            .fixedSize()
                        if let subtitle {
                            subtitle
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(accessibilityState)
        .accessibilityHint(destinationLabel)
        .accessibilityAddTraits(isHighlighted || isCurrent ? [.isSelected] : [])
    }

    private var hasStatus: Bool {
        isStopping || session.agentActivity != nil
    }

    @ViewBuilder
    private var status: some View {
        if isStopping {
            Text("Stopping…")
                .font(.footnote)
                .foregroundStyle(.secondary)
        } else if let activity = session.agentActivity {
            AgentActivityBadge(activity: activity)
        }
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
        if let activity = session.agentActivity { return activity.label }
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

/// A small capsule saying what a running agent is doing. "Needs you" is the
/// only one drawn to catch the eye; the others stay quiet.
struct AgentActivityBadge: View {
    let activity: AgentActivity

    var body: some View {
        Text(activity.label)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .foregroundStyle(foreground)
            .background(background, in: Capsule())
            .accessibilityHidden(true)
    }

    private var foreground: Color {
        switch activity {
        case .needsYou: .white
        case .working: .blue
        case .done: Self.doneGreen
        }
    }

    private var background: Color {
        switch activity {
        case .needsYou: .orange
        case .working: .blue.opacity(0.15)
        case .done: .green.opacity(0.15)
        }
    }

    /// System green is too light to read as caption text on its own tint in
    /// light mode (about 2:1), so light mode takes the darker shade the
    /// system uses at increased contrast. Dark mode reads fine as it is.
    private static let doneGreen = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark
            ? .systemGreen
            : UIColor(red: 0.141, green: 0.541, blue: 0.239, alpha: 1)
    })
}
