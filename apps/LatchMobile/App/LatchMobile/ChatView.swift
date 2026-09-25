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
/// explicit Take terminal item in the composer's "+" menu
/// (docs/DECISION_CONVERSATION_GEOMETRY.md).
struct ChatView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var appModel
    @State private var store: ConversationStore?
    @FocusState private var composerFocused: Bool
    @State private var takingTerminal = false
    @State private var showingDetails = false
    /// Set while the Stop confirmation is up; ending the agent's work is
    /// never one tap.
    @State private var confirmingStop = false
    @State private var explainingStopGrant = false

    /// The newest copy of this session the list has, so Stop disappears once
    /// the Mac reports the session ended.
    private var currentSession: SessionSummary {
        appModel.sessions.first { $0.id == session.id } ?? session
    }

    /// Nil until the store exists; the status chip then shows the Hub-derived
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
        // The navigation bar gives way to floating chrome: the transcript
        // scrolls beneath a back button and a status chip, and the chip opens
        // the session's details.
        .sessionChrome(
            session: currentSession,
            status: screenContent?.statusLine,
            showingDetails: $showingDetails,
            details: SessionDetailsActions(stop: stopRequest, isStopping: isStopping)
        ) {
            SessionChromeBar(
                title: session.headline.primary,
                status: screenContent?.statusLine,
                statusIdentifier: "conversation.toolbar.status",
                showDetails: { showingDetails = true }
            ) {
                if appModel.surface.terminal {
                    Button {
                        takingTerminal = true
                    } label: {
                        FloatingControl(systemImage: "terminal")
                            .foregroundStyle(.primary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Switch to terminal")
                    .accessibilityHint("Takes this session's terminal from the Mac")
                    .accessibilityIdentifier("conversation.toolbar.terminal")
                } else {
                    EmptyView()
                }
            }
        }
        .navigationDestination(isPresented: $takingTerminal) {
            TerminalView(session: session, autoAttach: currentSession.isRunning)
        }
        .sessionStopPrompts(
            session: currentSession,
            confirming: $confirmingStop,
            explainingGrant: $explainingStopGrant
        )
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
                composerFocused: $composerFocused,
                placeholder: session.connector.composerPlaceholder,
                sessionActions: sessionActions,
                attachments: attachmentControls(store)
            )
        }
    }

    /// The composer's "+" menu, holding only what this phone can do here.
    /// Stop stays offered without the grant so the alert can say what to
    /// change on the Mac, the same bargain the session list makes.
    private var sessionActions: ConversationSessionActions {
        var actions = ConversationSessionActions(sessionLink: SessionDeepLink.url(forSession: session.id))
        if appModel.surface.terminal {
            actions.takeTerminal = { takingTerminal = true }
        }
        actions.stop = stopRequest
        actions.isStopping = isStopping
        return actions
    }

    /// Files for the next message. Offered only when the Mac serves the
    /// attachments route and this device may write messages; otherwise `add`
    /// stays nil and the "+" menu shows no attachment items.
    private func attachmentControls(_ store: ConversationStore) -> ConversationAttachmentControls {
        var controls = ConversationAttachmentControls(
            items: store.attachments,
            phase: store.attachmentPhase,
            remove: { store.removeAttachment($0) }
        )
        if appModel.attachmentLimit != nil {
            let limit = appModel.attachmentLimit
            controls.add = { store.addAttachment($0, maximumBytes: limit) }
        }
        return controls
    }

    private var stopRequest: (() -> Void)? {
        appModel.stopRequest(
            for: currentSession,
            confirm: { confirmingStop = true },
            explainGrant: { explainingStopGrant = true }
        )
    }

    private var isStopping: Bool {
        appModel.stoppingSessionIDs.contains(session.id)
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
