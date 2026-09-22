import LatchMobileKit
import SwiftUI

/// Hub-owned conversation rendering. The view does not fold transcript records
/// or decide whether an interaction is allowed; those answers arrive in the
/// store's pushed state over the one v2 socket.
///
/// This file owns the session lifecycle and the terminal fallbacks. What the
/// conversation looks like lives in `Conversation/`, and how wire items become
/// turns lives in `ConversationProjection`.
///
/// Opening a conversation observes the session; it does not take it. No
/// terminal socket is opened, the PTY is not resized, and the owner check is
/// not raised: sends and answers travel through the Hub, which validates them
/// against the screen at the last moment. The terminal is taken only by the
/// explicit toolbar link (docs/DECISION_CONVERSATION_GEOMETRY.md).
struct ChatView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var appModel
    @State private var store: ConversationStore?
    @FocusState private var composerFocused: Bool

    /// Nil until the store exists; the toolbar then shows the Hub-derived
    /// status, including the newest running tool when appropriate.
    @MainActor
    private var screenContent: ConversationScreenContent? {
        store.map(ConversationScreenContent.init(store:))
    }

    var body: some View {
        Group {
            if let store {
                conversation(store)
            } else if appModel.surface.chat {
                ProgressView("Opening conversation…")
            } else {
                terminalFallback(
                    title: "Conversation unavailable",
                    detail: "This Mac does not offer the v2 Conversation Hub."
                )
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ConversationToolbar(title: session.displayName, statusLine: screenContent?.statusLine)
            if appModel.surface.terminal {
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink {
                        TerminalView(session: session, autoAttach: session.isRunning)
                    } label: {
                        Image(systemName: "terminal")
                    }
                    .accessibilityLabel("Take terminal")
                    .accessibilityHint("Opens the live terminal here and detaches it from your Mac.")
                }
            }
        }
        .task {
            guard store == nil else { return }
            store = appModel.conversationStore(for: session)
            store?.start()
        }
    }

    @ViewBuilder
    private func conversation(_ store: ConversationStore) -> some View {
        if ConversationSupport.derive(state: store.state) == .unavailable {
            terminalFallback(
                title: "Conversation unsupported",
                detail: "This session's connector cannot provide a conversation."
            )
        } else {
            ConversationScreen(
                content: screenContent ?? ConversationScreenContent(store: store),
                actions: ConversationScreenActions(store: store),
                draft: Binding(get: { store.draft }, set: { store.draft = $0 }),
                composerFocused: $composerFocused
            )
        }
    }

    /// Both dead ends keep their explanation and gain a way out.
    ///
    /// Telling someone to walk to their Mac, on a session that is live,
    /// reachable and already authenticated, is what this feature exists to
    /// remove — so the button appears whenever this device's grant and the
    /// Mac's routes actually allow a terminal, and the old sentence about
    /// attaching on the Mac stays only for the case where they do not.
    @ViewBuilder
    private func terminalFallback(title: String, detail: String) -> some View {
        if appModel.surface.terminal {
            ContentUnavailableView {
                Label(title, systemImage: "terminal")
            } description: {
                Text(detail + " You can take the session's terminal here instead; that detaches it from your Mac.")
            } actions: {
                NavigationLink("Take terminal") {
                    TerminalView(session: session, autoAttach: session.isRunning)
                }
                .buttonStyle(.borderedProminent)
            }
        } else {
            ContentUnavailableView(
                title,
                systemImage: "terminal",
                description: Text(detail + " Use `latch attach` on the Mac for this session.")
            )
        }
    }
}
