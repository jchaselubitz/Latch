import LatchMobileKit
import SwiftUI

/// Why neither screen can be opened, said as what to do rather than what
/// failed.
struct SessionUnavailableView: View {
    let session: SessionSummary
    let block: SessionRouteBlock

    var body: some View {
        Group {
            switch block {
            case .needsControlGrant:
                // The preview needs only `observe`, so an observing phone may
                // read the pane. Showing it behind the explanation is the
                // difference between an explanation and a dead end.
                VStack(spacing: 0) {
                    TerminalStillView(session: session)
                    VStack(spacing: 8) {
                        Text("This phone can't open a terminal")
                            .font(.headline)
                        Text(
                            """
                            This phone does not currently have terminal access. Open Latch on your \
                            Mac, find this phone under Remote Access, set it to Control, and turn on \
                            Allow terminal.
                            """
                        )
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(16)
                    .background(.bar)
                }
            case .noTerminalEndpoint:
                MessageView(
                    icon: "arrow.up.circle",
                    title: "This Mac has no terminal route",
                    detail: """
                    This session has no conversation connector, and the Mac is older than the \
                    terminal route that would stand in for one. Update Latch on the Mac.
                    """
                )
            case .noConversation:
                MessageView(
                    icon: "terminal",
                    title: "Nothing to open",
                    detail: """
                    This Mac offers neither the Conversation Hub nor a terminal route. Use \
                    `latch attach` on the Mac for this session.
                    """
                )
            }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }
}
