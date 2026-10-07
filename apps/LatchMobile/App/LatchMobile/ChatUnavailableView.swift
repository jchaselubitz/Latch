import LatchMobileKit
import SwiftUI

/// A Chat choice never opens a terminal by implication. When Chat cannot open,
/// this screen says why and makes terminal takeover a separate, deliberate
/// action when the Mac and this phone permit it.
struct ChatUnavailableView: View {
    let session: SessionSummary
    let block: ChatRouteBlock

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: "exclamationmark.bubble")
        } description: {
            Text(detail)
        } actions: {
            recovery
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var title: String {
        switch block {
        case .noConnector: "Chat isn't available for this session"
        case .noConversationEndpoint: "Chat needs a newer Mac service"
        }
    }

    private var detail: String {
        switch block {
        case .noConnector:
            "This session is a shell or was started without a recognized agent connector. Start a new Claude Code or Codex session to use Chat."
        case .noConversationEndpoint:
            "This Mac's Latch service does not offer the Conversation Hub. Update Latch on the Mac, then reopen this session."
        }
    }

    @ViewBuilder
    private var recovery: some View {
        switch terminalRecovery {
        case .available:
            NavigationLink("Take terminal") {
                // Reaching this screen began with a Chat tap. The terminal is
                // only taken after this second, explicit tap.
                TerminalView(session: session, autoAttach: false)
                    .pushedOverSessionColumn()
            }
            .buttonStyle(.borderedProminent)
        case .needsControlGrant:
            Text("To take the terminal instead, set this phone to Control and enable Allow terminal in Latch on your Mac.")
                .multilineTextAlignment(.center)
        case .unavailable:
            Text("Use `latch attach` on the Mac for this session.")
        }
    }

    private var terminalRecovery: TerminalRecovery {
        switch block {
        case .noConnector(let recovery), .noConversationEndpoint(let recovery): recovery
        }
    }
}
