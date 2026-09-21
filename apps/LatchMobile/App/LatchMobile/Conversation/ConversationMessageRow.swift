import LatchMobileKit
import SwiftUI

/// A person's message is a compact trailing bubble; the agent's is document
/// content across the full width.
struct ConversationMessageRow: View, Equatable {
    let message: ConversationMessagePresentation

    var body: some View {
        switch message.role {
        case .user:
            VStack(alignment: .trailing, spacing: 3) {
                Text(message.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Color.accentColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                if let delivery = message.deliveryCaption {
                    Text(delivery)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 38)
        case .assistant, .other:
            ConversationMarkdownView(text: message.text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Content a newer Mac sent that this build cannot present.
struct ConversationUnrecognizedRow: View {
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "questionmark.square.dashed")
            Text("This item needs a newer version of Latch to display.")
            Spacer(minLength: 0)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}
