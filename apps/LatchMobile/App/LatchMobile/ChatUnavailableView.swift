import LatchMobileKit
import SwiftUI

/// A Chat choice never opens a terminal by implication. When Chat cannot open,
/// this screen says why and makes terminal takeover a separate, deliberate
/// action when the Mac and this phone permit it.
struct ChatUnavailableView: View {
    let session: SessionSummary
    let block: ChatRouteBlock

    var body: some View {
        Group {
            if case .awaitingLink = block {
                // Not a diagnosis: the Mac has not been heard from on this
                // link yet. Lead with the reconnection; the update hint sits
                // underneath because it only applies if the link returns and
                // the Mac still offers no Chat.
                LinkPendingView(
                    possibleCause: "If Chat still doesn't open once your Mac reconnects, Latch on the Mac may need an update."
                )
            } else {
                ContentUnavailableView {
                    Label(title, systemImage: "exclamationmark.bubble")
                } description: {
                    Text(detail)
                } actions: {
                    recovery
                }
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var title: String {
        switch block {
        case .noConnector: "Chat isn't available for this session"
        case .noConversationEndpoint: "Chat needs a newer Mac service"
        case .awaitingLink: "Reconnecting"
        }
    }

    private var detail: String {
        switch block {
        case .noConnector:
            "This session is a shell or was started without a recognized agent connector. Start a new Claude Code or Codex session to use Chat."
        case .noConversationEndpoint:
            "This Mac's Latch service does not offer the Conversation Hub. Update Latch on the Mac, then reopen this session."
        case .awaitingLink:
            "Restoring the secure connection to your Mac."
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
        case .awaitingLink: .unavailable
        }
    }
}

/// Shown in place of a session screen while the link to the Mac is being
/// established or restored. The link state is the headline; `possibleCause`
/// is a quieter line for what it could mean if reconnecting does not help.
/// The route recomputes when the link is ready, so this screen gives way on
/// its own to the session or to a definite explanation.
struct LinkPendingView: View {
    let possibleCause: String

    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            VStack(spacing: 12) {
                indicator
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                if showsPossibleCause {
                    Text(possibleCause)
                        .font(.footnote)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                        .padding(.top, 12)
                        .accessibilityIdentifier("linkPending.possibleCause")
                }
            }
            .padding(32)
            .frame(maxWidth: .infinity)
            .padding(.top, 60)
        }
        .accessibilityIdentifier("linkPending")
    }

    /// Still trying: a spinner. Stopped trying: a still icon.
    @ViewBuilder
    private var indicator: some View {
        if isRetrying {
            ProgressView()
                .controlSize(.large)
                .frame(height: 44)
        } else {
            Image(systemName: icon)
                .font(.largeTitle)
                .foregroundStyle(.secondary)
        }
    }

    private var isRetrying: Bool {
        switch model.linkState {
        case .connecting, .interrupted, .macOffline: true
        default: false
        }
    }

    /// The update hint only fits while the answer is unknown. A definite
    /// mismatch already names its remedy, and a revoked pairing is not a
    /// version problem.
    private var showsPossibleCause: Bool {
        switch model.linkState {
        case .connecting, .interrupted, .macOffline, .failed: true
        default: false
        }
    }

    private var icon: String {
        switch model.linkState {
        case .incompatible(let mismatch): mismatch.icon
        case .revoked: "xmark.shield"
        case .pairingRequired: "qrcode"
        case .unlinked: "link"
        default: "exclamationmark.triangle"
        }
    }

    private var title: String {
        switch model.linkState {
        case .connecting: "Connecting"
        case .interrupted(let link, _): SessionsView.interruptedTitle(link)
        case .macOffline: "Waiting for your Mac"
        case .failed: "Cannot reach your Mac"
        case .incompatible(let mismatch): mismatch.title
        case .revoked: "This phone was unpaired"
        case .pairingRequired: "Pair again"
        case .unlinked: "Not connected"
        case .linked: "Opening"
        }
    }

    private var detail: String {
        switch model.linkState {
        case .connecting: "Connecting securely to your Mac."
        case .interrupted(let link, _): SessionsView.interruptedDetail(link)
        case .macOffline:
            "Your Mac isn't reachable through the relay. It may be asleep or have remote access turned off. This phone keeps checking."
        case .failed(let reason), .revoked(let reason), .pairingRequired(let reason): reason
        case .incompatible(let mismatch): mismatch.detail
        case .unlinked: "This phone isn't connected to a Mac."
        case .linked: ""
        }
    }
}
