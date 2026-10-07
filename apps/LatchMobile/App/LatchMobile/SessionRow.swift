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
