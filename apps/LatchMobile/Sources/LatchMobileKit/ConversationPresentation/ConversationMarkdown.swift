import Foundation

/// The Markdown seam for agent prose.
///
/// Decision (coo:1034.58an), taken against the captured transcript corpus in
/// `fixtures/conversation`: its assistant messages use paragraphs, `##` headings, bullet and
/// numbered lists, GitHub tables, fenced code with long lines, inline code,
/// bold, and bare URLs. They contain no raw HTML and no images. SwiftUI's
/// `Text` renders only inline attributes, so block structure (fences, lists,
/// tables) would be flattened by attributed Markdown alone. The chosen path is
/// therefore both native pieces together, with no third-party dependency:
///
/// - this small block parser splits a message into paragraphs, headings,
///   lists, tables, quotes, rules, and fenced code; and
/// - Foundation's `AttributedString(markdown:)` in inline-only mode renders
///   the prose inside each block.
///
/// Safety properties, each covered by `ConversationMarkdownTests`:
/// - Raw HTML is never interpreted; Foundation keeps it as literal text.
/// - Image syntax keeps its alt text and loses its URL, so nothing is fetched.
/// - Only `http`, `https`, and `mailto` links survive; any other scheme is
///   reduced to its text.
/// - Every word of the source reaches a rendered string, so nothing becomes
///   unselectable by being dropped.
public enum ConversationMarkdown {
    public enum Block: Equatable, Sendable {
        case paragraph(String)
        case heading(level: Int, text: String)
        /// Whitespace-exact; never passed through the inline parser.
        case code(language: String?, code: String)
        case list([ListItem])
        case table(header: [String], rows: [[String]])
        case quote(String)
        case rule
    }

    public struct ListItem: Equatable, Sendable {
        /// Nesting depth, from the item's indentation.
        public let depth: Int
        /// `•` for bullets; the source number and delimiter for ordered items.
        public let marker: String
        public let text: String

        public init(depth: Int, marker: String, text: String) {
            self.depth = depth
            self.marker = marker
            self.text = text
        }
    }

    // MARK: Blocks

    public static func blocks(from source: String) -> [Block] {
        let lines = source
            .replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map(String.init)
        var blocks: [Block] = []
        var paragraph: [String] = []
        var index = 0

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            blocks.append(.paragraph(paragraph.joined(separator: "\n")))
            paragraph.removeAll()
        }

        while index < lines.count {
            let line = lines[index]
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                flushParagraph()
                index += 1
                continue
            }
            if let fence = Fence(line) {
                flushParagraph()
                index = fence.consume(lines, from: index, into: &blocks)
                continue
            }
            if let heading = heading(line) {
                flushParagraph()
                blocks.append(heading)
                index += 1
                continue
            }
            if isRule(line) {
                flushParagraph()
                blocks.append(.rule)
                index += 1
                continue
            }
            if index + 1 < lines.count, line.contains("|"), isTableSeparator(lines[index + 1]) {
                flushParagraph()
                index = consumeTable(lines, from: index, into: &blocks)
                continue
            }
            if quoteBody(line) != nil {
                flushParagraph()
                var quoted: [String] = []
                while index < lines.count, let body = quoteBody(lines[index]) {
                    quoted.append(body)
                    index += 1
                }
                blocks.append(.quote(quoted.joined(separator: "\n")))
                continue
            }
            if listItem(line) != nil {
                flushParagraph()
                index = consumeList(lines, from: index, into: &blocks)
                continue
            }
            paragraph.append(line)
            index += 1
        }
        flushParagraph()
        return blocks
    }

    private struct Fence {
        let indent: Int
        let marker: Character
        let length: Int
        let language: String?

        init?(_ line: String) {
            let indent = line.prefix { $0 == " " || $0 == "\t" }.count
            let rest = line.dropFirst(indent)
            guard let marker = rest.first, marker == "`" || marker == "~" else { return nil }
            let length = rest.prefix { $0 == marker }.count
            guard length >= 3 else { return nil }
            let info = rest.dropFirst(length).trimmingCharacters(in: .whitespaces)
            // A backtick fence's info string cannot itself contain a backtick;
            // such a line is inline code, not a fence.
            if marker == "`", info.contains("`") { return nil }
            self.indent = indent
            self.marker = marker
            self.length = length
            let word = info.split(separator: " ").first.map(String.init)
            language = word?.isEmpty == false ? word : nil
        }

        func closes(_ line: String) -> Bool {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard trimmed.count >= length else { return false }
            return trimmed.allSatisfy { $0 == marker }
        }

        /// An unterminated fence runs to the end of the message, as in
        /// CommonMark; a streaming reply shows its code as code immediately.
        func consume(_ lines: [String], from start: Int, into blocks: inout [Block]) -> Int {
            var body: [String] = []
            var index = start + 1
            while index < lines.count {
                if closes(lines[index]) {
                    index += 1
                    break
                }
                body.append(stripIndent(lines[index]))
                index += 1
            }
            blocks.append(.code(language: language, code: body.joined(separator: "\n")))
            return index
        }

        private func stripIndent(_ line: String) -> String {
            var removed = 0
            var result = Substring(line)
            while removed < indent, let first = result.first, first == " " || first == "\t" {
                result = result.dropFirst()
                removed += 1
            }
            return String(result)
        }
    }

    private static func heading(_ line: String) -> Block? {
        let trimmed = line.drop { $0 == " " }
        guard line.count - trimmed.count <= 3 else { return nil }
        let hashes = trimmed.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes) else { return nil }
        let rest = trimmed.dropFirst(hashes)
        guard rest.isEmpty || rest.first == " " else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        // A closing run of #'s is decoration.
        while text.hasSuffix("#") { text.removeLast() }
        return .heading(level: hashes, text: text.trimmingCharacters(in: .whitespaces))
    }

    private static func isRule(_ line: String) -> Bool {
        let compact = line.filter { $0 != " " && $0 != "\t" }
        guard compact.count >= 3, let first = compact.first, "-*_".contains(first) else { return false }
        return compact.allSatisfy { $0 == first }
    }

    private static func quoteBody(_ line: String) -> String? {
        let trimmed = line.drop { $0 == " " }
        guard line.count - trimmed.count <= 3, trimmed.first == ">" else { return nil }
        let body = trimmed.dropFirst()
        return String(body.first == " " ? body.dropFirst() : body)
    }

    // MARK: Lists

    private struct ParsedItem {
        let indent: Int
        let marker: String
        let text: String
    }

    private static func listItem(_ line: String) -> ParsedItem? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }.count
        let rest = line.dropFirst(indent)
        guard let first = rest.first else { return nil }
        if "-*+".contains(first) {
            let after = rest.dropFirst()
            guard after.first == " " else { return nil }
            return ParsedItem(indent: indent, marker: "•", text: after.trimmingCharacters(in: .whitespaces))
        }
        let digits = rest.prefix { $0.isASCII && $0.isNumber }
        guard (1...9).contains(digits.count) else { return nil }
        let after = rest.dropFirst(digits.count)
        guard let delimiter = after.first, delimiter == "." || delimiter == ")",
              after.dropFirst().first == " "
        else { return nil }
        return ParsedItem(
            indent: indent,
            marker: String(digits) + String(delimiter),
            text: after.dropFirst().trimmingCharacters(in: .whitespaces)
        )
    }

    private static func consumeList(_ lines: [String], from start: Int, into blocks: inout [Block]) -> Int {
        var parsed: [ParsedItem] = []
        var index = start
        while index < lines.count {
            let line = lines[index]
            if let item = listItem(line) {
                parsed.append(item)
                index += 1
                continue
            }
            let isBlank = line.trimmingCharacters(in: .whitespaces).isEmpty
            if isBlank {
                // A blank line continues the list only when the list resumes
                // or the next line is indented under the current item.
                guard index + 1 < lines.count else { break }
                let next = lines[index + 1]
                let nextIndent = next.prefix { $0 == " " || $0 == "\t" }.count
                if listItem(next) != nil || (nextIndent >= 2 && Fence(next) == nil && !next.trimmingCharacters(in: .whitespaces).isEmpty) {
                    index += 1
                    continue
                }
                break
            }
            // Another block type ends the list; anything else is a lazy
            // continuation of the current item.
            if Fence(line) != nil || heading(line) != nil || isRule(line) || quoteBody(line) != nil { break }
            if index + 1 < lines.count, line.contains("|"), isTableSeparator(lines[index + 1]) { break }
            guard let last = parsed.popLast() else { break }
            parsed.append(ParsedItem(
                indent: last.indent,
                marker: last.marker,
                text: last.text + "\n" + line.trimmingCharacters(in: .whitespaces)
            ))
            index += 1
        }

        // Depth follows distinct indentation levels, so two- and four-space
        // nesting both read as one level per step.
        var levels: [Int] = []
        let items = parsed.map { item -> ListItem in
            while let last = levels.last, last > item.indent { levels.removeLast() }
            if levels.last != item.indent { levels.append(item.indent) }
            return ListItem(depth: levels.count - 1, marker: item.marker, text: item.text)
        }
        blocks.append(.list(items))
        return index
    }

    // MARK: Tables

    private static func isTableSeparator(_ line: String) -> Bool {
        let cells = tableCells(line)
        guard line.contains("-"), !cells.isEmpty else { return false }
        return cells.allSatisfy { cell in
            let core = cell.trimmingCharacters(in: CharacterSet(charactersIn: ":"))
            return !core.isEmpty && core.allSatisfy { $0 == "-" }
        }
    }

    private static func consumeTable(_ lines: [String], from start: Int, into blocks: inout [Block]) -> Int {
        let header = tableCells(lines[start])
        var rows: [[String]] = []
        var index = start + 2
        while index < lines.count {
            let line = lines[index]
            guard line.contains("|"), !line.trimmingCharacters(in: .whitespaces).isEmpty else { break }
            rows.append(tableCells(line))
            index += 1
        }
        blocks.append(.table(header: header, rows: rows))
        return index
    }

    /// Splits on unescaped pipes outside inline code.
    static func tableCells(_ line: String) -> [String] {
        var trimmed = Substring(line.trimmingCharacters(in: .whitespaces))
        if trimmed.hasPrefix("|") { trimmed = trimmed.dropFirst() }
        if trimmed.hasSuffix("|"), !trimmed.hasSuffix("\\|") { trimmed = trimmed.dropLast() }
        var cells: [String] = []
        var current = ""
        var inCode = false
        var previous: Character?
        for character in trimmed {
            if character == "`" { inCode.toggle() }
            if character == "|", !inCode, previous != "\\" {
                cells.append(current.trimmingCharacters(in: .whitespaces))
                current = ""
            } else {
                current.append(character)
            }
            previous = character
        }
        cells.append(current.trimmingCharacters(in: .whitespaces))
        return cells
    }

    // MARK: Inline

    /// Schemes a rendered link may open. Everything else keeps its text only.
    public static let allowedLinkSchemes: Set<String> = ["http", "https", "mailto"]

    /// Inline prose: emphasis, strong, code, strikethrough, and safe links.
    /// Whitespace and line breaks are preserved; a parse failure falls back
    /// to the literal text rather than to nothing.
    public static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: false,
            interpretedSyntax: .inlineOnlyPreservingWhitespace,
            failurePolicy: .returnPartiallyParsedIfPossible
        )
        guard var attributed = try? AttributedString(markdown: text, options: options) else {
            return AttributedString(text)
        }
        for run in attributed.runs {
            if run.imageURL != nil {
                attributed[run.range].imageURL = nil
            }
            if let link = run.link, !allowedLinkSchemes.contains(link.scheme?.lowercased() ?? "") {
                attributed[run.range].link = nil
            }
        }
        return attributed
    }

    /// Everything a reader can see in a block, in order. Used to prove the
    /// renderer keeps every word of the source.
    public static func renderedText(of block: Block) -> String {
        switch block {
        case .paragraph(let text), .quote(let text):
            return String(inline(text).characters)
        case .heading(_, let text):
            return String(inline(text).characters)
        case .code(let language, let code):
            // The language is shown as the block's caption.
            return language.map { $0 + "\n" + code } ?? code
        case .list(let items):
            return items.map { $0.marker + " " + String(inline($0.text).characters) }.joined(separator: "\n")
        case .table(let header, let rows):
            return ([header] + rows)
                .map { $0.map { String(inline($0).characters) }.joined(separator: " ") }
                .joined(separator: "\n")
        case .rule:
            return ""
        }
    }
}
