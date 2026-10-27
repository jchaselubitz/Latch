import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        HStack(spacing: 11) {
            AgentLogo(agent: session.agent, size: 24)
                .opacity(session.state.isLive ? 1 : 0.65)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(session.name)
                        .fontWeight(.semibold)
                        .lineLimit(1)
                    if session.state != .running {
                        StatePill(state: session.state)
                    }
                }
                Text(subtitle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 3) {
                Text(session.abbreviatedDirectory)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                    .help(session.cwd)
                if let idle = session.displayIdleLabel {
                    Text(idle)
                        .font(.caption2)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(session.name), \(session.agent.title), \(session.state.label)")
    }

    /// The directory already has its own chip, so an untitled session shows
    /// only what it runs.
    private var subtitle: String {
        session.title == nil ? session.commandLabel : session.displaySubtitle
    }
}
