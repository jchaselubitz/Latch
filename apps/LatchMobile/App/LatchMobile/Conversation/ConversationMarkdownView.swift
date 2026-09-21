import LatchMobileKit
import SwiftUI
import UIKit

/// Renders agent prose through `ConversationMarkdown`: blocks natively,
/// inline constructs through Foundation's attributed Markdown.
///
/// Nothing here loads a resource: `Text` never fetches images, raw HTML is
/// already literal text, and links open only when tapped.
struct ConversationMarkdownView: View, Equatable {
    let text: String

    var body: some View {
        let blocks = ConversationMarkdown.blocks(from: text)
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                ConversationMarkdownBlock(block: block)
            }
        }
        .textSelection(.enabled)
    }
}

private struct ConversationMarkdownBlock: View {
    let block: ConversationMarkdown.Block

    var body: some View {
        switch block {
        case .paragraph(let text):
            Text(ConversationMarkdown.inline(text))
                .fixedSize(horizontal: false, vertical: true)
        case .heading(let level, let text):
            Text(ConversationMarkdown.inline(text))
                .font(level <= 2 ? .title3.weight(.semibold) : .headline)
                .accessibilityAddTraits(.isHeader)
        case .code(let language, let code):
            ConversationCodeBlock(language: language, code: code)
        case .list(let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(item.marker)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                        Text(ConversationMarkdown.inline(item.text))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case .table(let header, let rows):
            ScrollView(.horizontal) {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                            Text(ConversationMarkdown.inline(cell)).fontWeight(.semibold)
                        }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                                Text(ConversationMarkdown.inline(cell))
                            }
                        }
                    }
                }
                .font(.callout)
                .padding(.vertical, 2)
            }
        case .quote(let text):
            Text(ConversationMarkdown.inline(text))
                .foregroundStyle(.secondary)
                .padding(.leading, 10)
                .overlay(alignment: .leading) {
                    Rectangle().fill(.quaternary).frame(width: 3)
                }
        case .rule:
            Divider()
        }
    }
}

/// Whitespace-exact code that scrolls sideways instead of wrapping, with a
/// copy action for the whole block.
struct ConversationCodeBlock: View {
    let language: String?
    let code: String

    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                if let language {
                    Text(language)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .accessibilityLabel(copied ? "Copied" : "Copy code")
                .accessibilityIdentifier("conversation.code.copy")
            }
            .padding(.horizontal, 10)
            .padding(.top, 6)

            ScrollView(.horizontal) {
                Text(code)
                    .font(.footnote.monospaced())
                    .fixedSize(horizontal: true, vertical: true)
                    .padding(10)
            }
        }
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}
