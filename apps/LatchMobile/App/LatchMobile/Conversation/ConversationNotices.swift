import LatchMobileKit
import SwiftUI

/// Connection trouble, or content this build could not place, pinned above
/// the transcript.
struct ConversationConnectionBanner: View {
    let error: String?
    let skippedUpdates: Int

    var body: some View {
        if let error {
            banner(systemImage: "antenna.radiowaves.left.and.right", text: error)
        } else if skippedUpdates > 0 {
            // Placeholder rows already show unrecognized items in place;
            // content that could not be placed at all is only visible here.
            banner(
                systemImage: "arrow.down.app.dashed",
                text: "Some updates need a newer version of Latch (\(skippedUpdates) skipped)."
            )
            .foregroundStyle(.secondary)
        }
    }

    private func banner(systemImage: String, text: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: systemImage)
            Text(text).lineLimit(2)
            Spacer(minLength: 0)
        }
        .font(.caption2)
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial)
    }
}

/// Sends that did not simply succeed. A refused send and an uncertain one
/// look different and offer different ways out: a refused message was not
/// delivered, so its text goes back to the composer; an uncertain one may
/// have arrived, so sending it again is labelled as a new message. Every way
/// out that sends creates a new operation; nothing is replayed on its own.
struct ConversationOperationRows: View {
    let operations: [ConversationOperationPresentation]
    let actions: ConversationScreenActions

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(operations) { operation in
                ConversationOperationRow(operation: operation, actions: actions)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.thinMaterial)
    }
}

struct ConversationOperationRow: View {
    let operation: ConversationOperationPresentation
    let actions: ConversationScreenActions

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            VStack(alignment: .leading, spacing: 2) {
                Label(operation.title, systemImage: symbol)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(tint)
                Text(operation.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(operation.text)
                    .font(.callout)
                    .lineLimit(2)
                    .textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)
            HStack(spacing: 8) {
                ForEach(operation.actions, id: \.self) { action in
                    button(for: action)
                }
                Spacer(minLength: 0)
            }
            .controlSize(.small)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.1), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityIdentifier("conversation.operation.\(kindName)")
    }

    @ViewBuilder
    private func button(for action: ConversationOperationPresentation.Action) -> some View {
        let label = operation.label(for: action)
        switch action {
        case .edit:
            Button(label) { actions.edit(operation.id) }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("conversation.operation.edit")
        case .sendAgain:
            Button(label) { actions.retry(operation.id) }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("conversation.operation.sendAgain")
        case .dismiss:
            Button(label) { actions.dismiss(operation.id) }
                .buttonStyle(.borderless)
                .accessibilityIdentifier("conversation.operation.dismiss")
        }
    }

    private var symbol: String {
        switch operation.kind {
        case .refused: "arrow.uturn.backward.circle"
        case .uncertain: "questionmark.circle"
        case .needsReview: "exclamationmark.circle"
        }
    }

    private var tint: Color {
        switch operation.kind {
        case .refused: .orange
        case .uncertain: .indigo
        case .needsReview: .secondary
        }
    }

    private var kindName: String {
        switch operation.kind {
        case .refused: "refused"
        case .uncertain: "uncertain"
        case .needsReview: "needsReview"
        }
    }
}
