import Foundation
import XCTest

@testable import LatchMobileKit

/// The Markdown seam against the captured corpus and against the inputs it
/// must never act on.
final class ConversationMarkdownTests: XCTestCase {
    typealias Block = ConversationMarkdown.Block

    // MARK: Captured corpus

    /// Every letter and digit of every captured assistant message reaches a
    /// rendered string, in order. Markdown punctuation may disappear; words
    /// may not, because a dropped word cannot be selected or copied.
    func testCorpusRendersEveryWordOfEveryAssistantMessage() throws {
        let messages = try Self.corpusAssistantMessages()
        XCTAssertGreaterThan(messages.count, 30)
        for (name, text) in messages {
            let blocks = ConversationMarkdown.blocks(from: text)
            let rendered = blocks.map(ConversationMarkdown.renderedText(of:)).joined(separator: "\n")
            XCTAssertEqual(
                Self.words(rendered),
                Self.words(Self.withoutLinkTargets(text)),
                "\(name): rendered text lost or reordered content"
            )
        }
    }

    func testCorpusStructureIsPreserved() throws {
        let blocks = try Self.corpusAssistantMessages().flatMap { ConversationMarkdown.blocks(from: $0.1) }

        let codes = blocks.compactMap { block -> String? in
            if case .code(_, let code) = block { return code }
            return nil
        }
        XCTAssertEqual(codes.count, 3, "fenced-code has three fences")
        XCTAssertTrue(codes.contains { $0.hasPrefix("node -e \"fetch(") && !$0.contains("\n") }, "long command stays one line")
        XCTAssertTrue(blocks.contains { if case .heading(2, "The fix") = $0 { true } else { false } })
        XCTAssertTrue(blocks.contains { if case .table(let header, let rows) = $0 { header.count == 3 && !rows.isEmpty } else { false } })
        XCTAssertTrue(blocks.contains { block in
            guard case .list(let items) = block else { return false }
            return items.first?.marker == "1." && items.count >= 2
        })
        XCTAssertTrue(blocks.contains { block in
            guard case .list(let items) = block else { return false }
            return items.first?.marker == "•"
        })
    }

    func testCorpusFencesAreWhitespaceExact() throws {
        let text = try XCTUnwrap(Self.corpusAssistantMessages().first { $0.0 == "fenced-code" }?.1)
        let blocks = ConversationMarkdown.blocks(from: text)
        XCTAssertTrue(blocks.contains(.code(language: nil, code: "[webapp] Overlord web server listening on :::8080")))
        XCTAssertTrue(blocks.contains { if case .code("bash", _) = $0 { true } else { false } })
    }

    /// Nothing in the corpus parses to a link outside the allowed schemes, and
    /// nothing carries an image URL.
    func testCorpusInlineAttributesAreSafe() throws {
        for (_, text) in try Self.corpusAssistantMessages() {
            for block in ConversationMarkdown.blocks(from: text) {
                for string in Self.inlineStrings(of: block) {
                    let attributed = ConversationMarkdown.inline(string)
                    for run in attributed.runs {
                        XCTAssertNil(run.imageURL)
                        if let link = run.link {
                            XCTAssertTrue(ConversationMarkdown.allowedLinkSchemes.contains(link.scheme ?? ""), "\(link)")
                        }
                    }
                }
            }
        }
    }

    // MARK: Safety

    func testRawHTMLIsLiteralText() {
        let source = #"before <script>alert(1)</script> <img src="https://example.com/x.png"> after"#
        let blocks = ConversationMarkdown.blocks(from: source)
        XCTAssertEqual(blocks, [.paragraph(source)])
        let rendered = ConversationMarkdown.inline(source)
        XCTAssertEqual(String(rendered.characters), source)
        XCTAssertTrue(rendered.runs.allSatisfy { $0.link == nil && $0.imageURL == nil })
    }

    func testAnHTMLBlockIsAParagraphNotMarkup() {
        let source = "<div onclick=\"x()\">\n<iframe src=\"https://example.com\"></iframe>\n</div>"
        XCTAssertEqual(ConversationMarkdown.blocks(from: source), [.paragraph(source)])
    }

    func testImagesKeepAltTextAndLoseTheirURL() {
        let rendered = ConversationMarkdown.inline("see ![build graph](https://example.com/graph.png) here")
        XCTAssertEqual(String(rendered.characters), "see build graph here")
        XCTAssertTrue(rendered.runs.allSatisfy { $0.imageURL == nil })
    }

    func testOnlyWebAndMailLinksSurvive() {
        let rendered = ConversationMarkdown.inline(
            "[a](https://example.com) [b](javascript:alert(1)) [c](file:///etc/passwd) [d](mailto:x@example.com) [e](latch://pair)"
        )
        let links = rendered.runs.compactMap { run in run.link.map { (String(rendered[run.range].characters), $0.absoluteString) } }
        XCTAssertEqual(links.map(\.0), ["a", "d"])
        XCTAssertEqual(String(rendered.characters), "a b c d e")
    }

    func testRepositoryPathsStayPlainText() {
        let rendered = ConversationMarkdown.inline("Edited `apps/LatchMobile/App/LatchMobile/ChatView.swift:147` and docs/README.md")
        XCTAssertTrue(rendered.runs.allSatisfy { $0.link == nil })
    }

    // MARK: Blocks

    func testFenceVariants() {
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "~~~swift\nlet a = 1\n    indented\n~~~"),
            [.code(language: "swift", code: "let a = 1\n    indented")]
        )
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "````\n```\ninner\n```\n````"),
            [.code(language: nil, code: "```\ninner\n```")]
        )
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "text\n```sh\nstill streaming"),
            [.paragraph("text"), .code(language: "sh", code: "still streaming")]
        )
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "```not a fence``` inline"),
            [.paragraph("```not a fence``` inline")]
        )
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "1. step\n   ```\n   code\n   ```"),
            [.list([.init(depth: 0, marker: "1.", text: "step")]), .code(language: nil, code: "code")]
        )
    }

    func testListsNestAndContinue() {
        let blocks = ConversationMarkdown.blocks(from: "- one\n  - nested\n    wraps\n- two\n\n  after blank\n\nparagraph")
        XCTAssertEqual(blocks, [
            .list([
                .init(depth: 0, marker: "•", text: "one"),
                .init(depth: 1, marker: "•", text: "nested\nwraps"),
                .init(depth: 0, marker: "•", text: "two\nafter blank"),
            ]),
            .paragraph("paragraph"),
        ])
    }

    func testTablesSplitCellsOutsideInlineCode() {
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "| A | B |\n|:---|---:|\n| `a|b` | c \\| d |\n\nafter"),
            [.table(header: ["A", "B"], rows: [["`a|b`", #"c \| d"#]]), .paragraph("after")]
        )
    }

    func testHeadingsQuotesAndRules() {
        XCTAssertEqual(
            ConversationMarkdown.blocks(from: "## Title ##\n> quoted\n> more\n\n---\n#hashtag"),
            [.heading(level: 2, text: "Title"), .quote("quoted\nmore"), .rule, .paragraph("#hashtag")]
        )
    }

    func testParagraphsKeepSoftLineBreaks() {
        XCTAssertEqual(ConversationMarkdown.blocks(from: "a\nb\n\nc"), [.paragraph("a\nb"), .paragraph("c")])
        XCTAssertEqual(String(ConversationMarkdown.inline("a\nb").characters), "a\nb")
    }

    // MARK: Helpers

    private static func corpusAssistantMessages() throws -> [(String, String)] {
        try ConversationSchema.claudeCaseDirectories().flatMap { directory -> [(String, String)] in
            try ConversationSchema.snapshot(in: directory).items.compactMap { item in
                if case .message(let role, let text, _) = item.kind, role == "assistant" {
                    return (directory.lastPathComponent, text)
                }
                return nil
            }
        }
    }

    private static func inlineStrings(of block: Block) -> [String] {
        switch block {
        case .paragraph(let text), .quote(let text), .heading(_, let text): [text]
        case .list(let items): items.map(\.text)
        case .table(let header, let rows): header + rows.flatMap { $0 }
        case .code, .rule: []
        }
    }

    /// A Markdown link's target is carried as an attribute, not as text.
    private static func withoutLinkTargets(_ text: String) -> String {
        text.replacingOccurrences(of: #"\]\([^)\s]*\)"#, with: "]", options: .regularExpression)
    }

    private static func words(_ text: String) -> String {
        String(text.unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) }.map(Character.init))
    }
}
