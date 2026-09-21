import LatchMobileKit
import SwiftUI

/// Hub-owned conversation rendering. The view does not fold transcript records
/// or decide whether an interaction is allowed; those answers arrive in the
/// store's pushed state over the one v2 socket.
///
/// This file owns the session lifecycle and the terminal fallbacks. What the
/// conversation looks like lives in `Conversation/`, and how wire items become
/// turns lives in `ConversationProjection`.
struct ChatView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var appModel
    @State private var store: ConversationStore?
    @State private var claimedTerminal: TerminalSession?
    @State private var terminalDrain: Task<Void, Never>?
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
                    .accessibilityLabel("Open terminal")
                }
            }
        }
        .task {
            guard store == nil else { return }
            store = appModel.conversationStore(for: session)
            store?.start()
            await claimSessionSurface()
        }
        .onDisappear {
            terminalDrain?.cancel()
            terminalDrain = nil
            if claimedTerminal != nil {
                appModel.discardTerminal(for: session)
                claimedTerminal = nil
            }
        }
    }

    /// Opening a live chat is an explicit choice to continue that session on
    /// this phone. Claim its exclusive terminal surface as well, and drain the
    /// repaint stream even though chat renders from the Conversation Hub; a
    /// socket whose output nobody reads would eventually be evicted as slow.
    private func claimSessionSurface() async {
        guard claimedTerminal == nil,
              let terminal = await appModel.claimTerminalForChat(for: session)
        else { return }
        claimedTerminal = terminal
        terminalDrain = Task {
            for await _ in terminal.output {
                if Task.isCancelled { return }
            }
        }
    }

    @ViewBuilder
    private func conversation(_ store: ConversationStore) -> some View {
        if store.state?.connector == nil, store.socketState == .open {
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
                Text(detail + " The session's terminal can be opened here instead.")
            } actions: {
                NavigationLink("Open terminal") {
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
