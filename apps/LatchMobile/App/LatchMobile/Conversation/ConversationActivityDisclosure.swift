import LatchMobileKit
import SwiftUI

/// A run of tool calls as one line: the live call while running, a count once
/// settled. Settled runs start collapsed; expanding shows each call in order.
struct ConversationActivityDisclosure: View, Equatable {
    let group: ConversationActivityGroup

    @State private var isExpanded = false

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.group == rhs.group }

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(group.tools) { tool in
                    ConversationToolRow(tool: tool)
                }
            }
            .padding(.top, 4)
        } label: {
            HStack(spacing: 6) {
                if group.isRunning {
                    ProgressView().controlSize(.mini)
                } else {
                    Image(systemName: group.failedCount > 0 ? "exclamationmark.circle" : "checkmark.circle")
                }
                Text(group.summary)
                    .lineLimit(1)
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .tint(.secondary)
        .accessibilityIdentifier("conversation.activity.disclosure")
    }
}

private struct ConversationToolRow: View {
    let tool: ConversationToolPresentation

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol)
                .accessibilityLabel(statusLabel)
            VStack(alignment: .leading, spacing: 2) {
                Text(tool.name).font(.caption.weight(.medium))
                if !tool.summary.isEmpty {
                    Text(tool.summary)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 0)
        }
        .foregroundStyle(.secondary)
    }

    private var symbol: String {
        switch tool.status {
        case .running: "circle.dashed"
        case .succeeded: "checkmark.circle"
        case .failed: "xmark.circle"
        case .other: "circle"
        }
    }

    private var statusLabel: String {
        switch tool.status {
        case .running: "Running"
        case .succeeded: "Succeeded"
        case .failed: "Failed"
        case .other(let value): value
        }
    }
}
