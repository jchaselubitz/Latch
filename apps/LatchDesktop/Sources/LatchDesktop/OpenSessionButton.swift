import SwiftUI
import AppKit

/// A split button: clicking it opens the session the way Settings says, while the
/// attached menu offers every launch shape the preferred terminal understands.
struct OpenSessionButton: View {
    @ObservedObject var store: SessionStore
    let sessionID: String
    let isEnabled: Bool

    var body: some View {
        Menu {
            OpenBehaviorMenuItems(store: store, sessionID: sessionID, isEnabled: isEnabled)
        } label: {
            Label("Open in \(store.preferredTerminal.rawValue)", systemImage: "terminal")
                .frame(maxWidth: .infinity)
        } primaryAction: {
            Task { await store.open(sessionID) }
        }
        .menuStyle(.button)
        .latchProminentButtonStyle()
        .controlSize(.large)
        .frame(maxWidth: .infinity)
        .disabled(!isEnabled)
    }
}

struct ResumeSessionButton: View {
    @ObservedObject var store: SessionStore
    let sessionID: String

    var body: some View {
        Button {
            Task { await store.resume(sessionID) }
        } label: {
            Label(
                store.resumingSessionIDs.contains(sessionID) ? "Resuming…" : "Resume Chat",
                systemImage: "arrow.clockwise"
            )
            .frame(maxWidth: .infinity)
        }
        .latchProminentButtonStyle()
        .controlSize(.large)
        .disabled(!store.canResume(sessionID))
        .help(store.resumeExplanation(sessionID))
    }
}

/// The launch shapes offered by every Open control. Unavailable shapes stay visible
/// with the reason attached rather than disappearing.
struct OpenBehaviorMenuItems: View {
    @ObservedObject var store: SessionStore
    let sessionID: String
    let isEnabled: Bool

    var body: some View {
        if store.preferredTerminal.supportedOpenBehaviors.isEmpty {
            Text("Your argument template decides how this terminal opens.")
        } else {
            ForEach(TerminalOpenBehavior.allCases) { behavior in
                Button {
                    Task { await store.open(sessionID, behavior: behavior) }
                } label: {
                    Label(title(for: behavior), systemImage: behavior.systemImage)
                }
                .disabled(!isEnabled || !store.preferredTerminal.supports(behavior))
            }
        }
    }

    private func title(for behavior: TerminalOpenBehavior) -> String {
        if let reason = store.preferredTerminal.unsupportedReason(for: behavior) {
            return "\(behavior.label) — \(reason)"
        }
        return behavior == store.effectiveOpenBehavior ? "\(behavior.label) (Default)" : behavior.label
    }
}
