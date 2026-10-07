import SwiftUI
import AppKit
import UniformTypeIdentifiers

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
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .frame(maxWidth: .infinity)
        .disabled(!isEnabled)
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
