import LatchMobileKit
import SwiftUI

/// The session title with the live transcript status beneath it. The status
/// says only what the host reported: "Working" plus a running tool when one
/// exists, never "Thinking" or "Done".
struct ConversationToolbar: ToolbarContent {
    let title: String
    let statusLine: String?

    var body: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            VStack(spacing: 0) {
                Text(title)
                    .font(.headline)
                    .lineLimit(1)
                if let statusLine {
                    Text(statusLine)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("conversation.toolbar.status")
        }
    }
}
