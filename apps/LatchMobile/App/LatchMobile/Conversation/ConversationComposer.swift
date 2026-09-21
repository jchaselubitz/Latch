import LatchMobileKit
import SwiftUI

/// The ordinary composer. The field stays editable whatever the host says,
/// including while offline; only the send button follows availability, and
/// the reason it is off reads as a state, not an error.
struct ConversationComposer: View {
    @Binding var draft: String
    @FocusState.Binding var focused: Bool
    let presentation: ConversationComposerPresentation
    let send: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let notice = presentation.notice {
                ConversationSendNoticeView(notice: notice)
            }
            HStack(spacing: 8) {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.plain)
                    .focused($focused)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(.quaternary, in: Capsule())
                    .accessibilityIdentifier("conversation.composer.field")
                Button {
                    let text = draft
                    draft = ""
                    send(text)
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title2)
                }
                .disabled(!canSubmit)
                .accessibilityLabel("Send")
                .accessibilityHint(presentation.notice?.title ?? "")
                .accessibilityIdentifier("conversation.composer.send")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var canSubmit: Bool {
        presentation.canSend && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

/// Why sending is off. Agent states are quiet and informational; only a lost
/// connection or an unavailable host uses a warning symbol.
struct ConversationSendNoticeView: View {
    let notice: ConversationSendNotice

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: symbol)
            VStack(alignment: .leading, spacing: 1) {
                Text(notice.title).fontWeight(.medium)
                if let detail = notice.detail {
                    Text(detail)
                }
            }
            Spacer(minLength: 0)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 4)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.composer.notice")
    }

    private var symbol: String {
        switch notice.kind {
        case .agentState: "hourglass"
        case .connection: "wifi.slash"
        case .unavailable: "nosign"
        }
    }
}
