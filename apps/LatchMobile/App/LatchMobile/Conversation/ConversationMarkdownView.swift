import LatchMobileKit
import SwiftUI
import UIKit

/// Renders agent prose through `ConversationMarkdown`: blocks natively,
/// inline constructs through Foundation's attributed Markdown. Prose, lists
/// and code are `SelectableText`, so a reader can select a phrase instead of
/// copying a whole block.
///
/// Nothing here loads a resource: image references are dropped, raw HTML is
/// already literal text, and links open only when tapped.
struct ConversationMarkdownView: View, Equatable {
    let text: String
    var textStyle: UIFont.TextStyle = .body

    var body: some View {
        let blocks = ConversationMarkdown.blocks(from: text)
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                ConversationMarkdownBlock(block: block, textStyle: textStyle)
            }
        }
        .textSelection(.enabled)
    }
}

private struct ConversationMarkdownBlock: View {
    let block: ConversationMarkdown.Block
    let textStyle: UIFont.TextStyle

    var body: some View {
        switch block {
        case .paragraph(let text):
            SelectableText(ConversationMarkdown.inline(text), style: .init(textStyle: textStyle))
        case .heading(let level, let text):
            SelectableText(
                ConversationMarkdown.inline(text),
                style: level <= 2 ? .init(textStyle: .title3, weight: .semibold) : .init(textStyle: .headline)
            )
            .accessibilityAddTraits(.isHeader)
        case .code(let language, let code):
            ConversationCodeBlock(language: language, code: code)
        case .list(let items):
            // One text view for the whole list, so a selection can run
            // across items.
            SelectableText(
                lines: items.map {
                    SelectableText.Line(marker: $0.marker, depth: $0.depth, text: ConversationMarkdown.inline($0.text))
                },
                style: .init(textStyle: textStyle)
            )
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
            SelectableText(ConversationMarkdown.inline(text), style: .init(textStyle: textStyle, color: .secondaryLabel))
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
                SelectableText(AttributedString(code), style: .init(textStyle: .footnote, monospaced: true, wraps: false))
                    .padding(10)
            }
        }
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

/// Read-only text a reader can select any part of.
///
/// On iOS, `.textSelection(.enabled)` only offers to copy a whole `Text`. A
/// non-editable `UITextView` gives the system's selection handles, loupe and
/// edit menu (Copy, Look Up, Translate, Share) over any range.
struct SelectableText: UIViewRepresentable {
    /// A paragraph, optionally led by a list marker and indented by depth.
    struct Line: Equatable {
        var marker: String?
        var depth = 0
        var text: AttributedString
    }

    struct Style: Equatable {
        var textStyle: UIFont.TextStyle = .body
        var weight: UIFont.Weight = .regular
        var monospaced = false
        var color: UIColor = .label
        /// When false, lines keep their length and the view is as wide as
        /// its longest line; the caller scrolls it.
        var wraps = true

        func font(compatibleWith traits: UITraitCollection) -> UIFont {
            let preferred = UIFont.preferredFont(forTextStyle: textStyle, compatibleWith: traits)
            if monospaced {
                return .monospacedSystemFont(ofSize: preferred.pointSize, weight: weight)
            }
            return weight == .regular ? preferred : .systemFont(ofSize: preferred.pointSize, weight: weight)
        }
    }

    let lines: [Line]
    let style: Style

    init(_ text: AttributedString, style: Style = Style()) {
        self.init(lines: [Line(text: text)], style: style)
    }

    init(lines: [Line], style: Style = Style()) {
        self.lines = lines
        self.style = style
    }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.dataDetectorTypes = []
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(context.environment.dynamicTypeSize))
        let attributed = Self.attributed(lines, style: style, traits: traits)
        // Reassigning the text drops the reader's selection, so only do it
        // when the text changed.
        if !view.attributedText.isEqual(to: attributed) {
            view.attributedText = attributed
        }
        view.textContainer.lineBreakMode = style.wraps ? .byWordWrapping : .byClipping
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        let available = style.wraps ? (proposal.width ?? .greatestFiniteMagnitude) : .greatestFiniteMagnitude
        // `UITextView.sizeThatFits` answers with the width it was offered, so
        // the used width comes from the text and the height from the view.
        let used = uiView.attributedText.boundingRect(
            with: CGSize(width: available, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            context: nil
        ).width
        let width = min(available, ceil(used) + 1)
        let height = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        return CGSize(width: width, height: ceil(height))
    }

    static func attributed(_ lines: [Line], style: Style, traits: UITraitCollection) -> NSAttributedString {
        let font = style.font(compatibleWith: traits)
        let result = NSMutableAttributedString()
        for (index, line) in lines.enumerated() {
            let start = result.length
            let paragraph = NSMutableParagraphStyle()
            let indent = CGFloat(line.depth) * 16
            paragraph.firstLineHeadIndent = indent
            paragraph.headIndent = indent
            if !style.wraps {
                paragraph.lineBreakMode = .byClipping
            }
            if let marker = line.marker {
                let markerAttributes: [NSAttributedString.Key: Any] = [
                    .font: UIFont.monospacedDigitSystemFont(ofSize: font.pointSize, weight: .regular),
                    .foregroundColor: UIColor.secondaryLabel,
                ]
                // Wrapped lines hang under the item's text, not its marker.
                let column = indent + ceil((marker as NSString).size(withAttributes: markerAttributes).width) + 6
                paragraph.headIndent = column
                paragraph.tabStops = [NSTextTab(textAlignment: .natural, location: column)]
                result.append(NSAttributedString(string: marker + "\t", attributes: markerAttributes))
            }
            result.append(inline(line.text, font: font, color: style.color))
            if index < lines.count - 1 {
                paragraph.paragraphSpacing = 4
                result.append(NSAttributedString(string: "\n", attributes: [.font: font]))
            }
            result.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: start, length: result.length - start))
        }
        return result
    }

    /// Carries the inline Markdown a `Text` would draw: emphasis, code,
    /// strikethrough and links.
    private static func inline(_ text: AttributedString, font: UIFont, color: UIColor) -> NSAttributedString {
        let result = NSMutableAttributedString()
        for run in text.runs {
            let intent = run.inlinePresentationIntent ?? []
            var runFont = intent.contains(.code) ? UIFont.monospacedSystemFont(ofSize: font.pointSize, weight: .regular) : font
            var traits: UIFontDescriptor.SymbolicTraits = []
            if intent.contains(.stronglyEmphasized) { traits.insert(.traitBold) }
            if intent.contains(.emphasized) { traits.insert(.traitItalic) }
            if !traits.isEmpty,
               let descriptor = runFont.fontDescriptor.withSymbolicTraits(runFont.fontDescriptor.symbolicTraits.union(traits)) {
                runFont = UIFont(descriptor: descriptor, size: runFont.pointSize)
            }
            var attributes: [NSAttributedString.Key: Any] = [.font: runFont, .foregroundColor: color]
            if intent.contains(.strikethrough) {
                attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
            }
            if let link = run.link {
                attributes[.link] = link
            }
            result.append(NSAttributedString(string: String(text[run.range].characters), attributes: attributes))
        }
        return result
    }
}
