import LatchMobileKit
import SwiftUI

/// A person's message is a compact trailing bubble; the agent's is document
/// content across the full width. A user-role message that is harness markup
/// is drawn as a card: trailing when the person caused it (a shell or slash
/// command), full width when the agent's machinery did.
struct ConversationMessageRow: View, Equatable {
    let message: ConversationMessagePresentation

    var body: some View {
        switch message.role {
        case .user:
            if let envelope = message.envelope {
                envelopeRow(envelope)
            } else {
                bubble
            }
        case .assistant, .other:
            ConversationMarkdownView(text: message.text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private func envelopeRow(_ envelope: ConversationEnvelope) -> some View {
        switch envelope.origin {
        case .person:
            VStack(alignment: .trailing, spacing: 3) {
                ConversationEnvelopeCard(envelope: envelope)
                deliveryCaption
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 38)
        case .agent:
            ConversationEnvelopeCard(envelope: envelope)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private var deliveryCaption: some View {
        if let delivery = message.deliveryCaption {
            Text(delivery)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    private var bubble: some View {
            VStack(alignment: .trailing, spacing: 3) {
                SelectableText(AttributedString(message.text))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Color.accentColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                deliveryCaption
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 38)
    }
}

/// What a harness envelope says, as a card: what kind of event, its subject
/// and state, a sentence on what happened, the content it carried, and its
/// identifiers behind a disclosure.
struct ConversationEnvelopeCard: View {
    let envelope: ConversationEnvelope

    @State private var showsBody = false
    @State private var showsDetails = false

    /// Long results start folded so a finished agent does not push the
    /// conversation off screen.
    private static let foldedBodyLines = 6

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if let headline = envelope.headline {
                SelectableText(
                    AttributedString(headline),
                    style: .init(textStyle: .subheadline, color: envelope.tone == .failure ? .systemRed : .label)
                )
            }
            if let content = envelope.body {
                contentView(content)
            }
            if !envelope.details.isEmpty {
                DisclosureGroup(isExpanded: $showsDetails) {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(envelope.details) { detail in
                            VStack(alignment: .leading, spacing: 1) {
                                Text(detail.label)
                                    .font(.caption2.weight(.semibold))
                                    .foregroundStyle(.secondary)
                                Text(detail.value)
                                    .font(.caption.monospaced())
                                    .textSelection(.enabled)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 4)
                } label: {
                    Text("Details")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tint(.secondary)
            }
        }
        .padding(12)
        .background(background, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(accent.opacity(0.35), lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("conversation.envelope.\(envelope.tag)")
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: envelope.symbolName)
                .font(.body)
                .foregroundStyle(accent)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(envelope.eyebrow.uppercased())
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
                Text(envelope.title)
                    .font(isCommand ? .callout.monospaced().weight(.medium) : .callout.weight(.semibold))
                    .lineLimit(3)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
            if let status = envelope.status {
                Text(status)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .foregroundStyle(accent)
                    .background(accent.opacity(0.14), in: Capsule())
            }
        }
    }

    @ViewBuilder
    private func contentView(_ content: String) -> some View {
        let isLong = content.split(separator: "\n", omittingEmptySubsequences: false).count > Self.foldedBodyLines
            || content.count > 480
        let shown = isLong && !showsBody ? Self.folded(content) : content
        VStack(alignment: .leading, spacing: 4) {
            Group {
                if envelope.bodyIsCode {
                    SelectableText(AttributedString(shown), style: .init(textStyle: .caption1, monospaced: true))
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                } else {
                    ConversationMarkdownView(text: shown, textStyle: .subheadline)
                        .font(.subheadline)
                }
            }
            if isLong {
                Button(showsBody ? "Show less" : "Show more") { showsBody.toggle() }
                    .font(.caption)
                    .buttonStyle(.borderless)
            }
        }
    }

    private static func folded(_ content: String) -> String {
        let lines = content.split(separator: "\n", omittingEmptySubsequences: false)
        let head = lines.prefix(foldedBodyLines).joined(separator: "\n")
        return String(head.prefix(480)) + "…"
    }

    private var isCommand: Bool {
        envelope.kind == .shellCommand || envelope.kind == .slashCommand
    }

    private var accent: Color {
        switch envelope.tone {
        case .neutral: .secondary
        case .running: .blue
        case .success: .green
        case .failure: .red
        case .attention: .orange
        }
    }

    private var background: Color {
        switch envelope.origin {
        case .person: Color.accentColor.opacity(0.10)
        case .agent: Color.secondary.opacity(0.08)
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
