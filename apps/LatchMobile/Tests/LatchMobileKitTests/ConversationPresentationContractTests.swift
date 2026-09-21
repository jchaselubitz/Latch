import Foundation
import XCTest

@testable import LatchMobileKit

/// Guards the hand-written presentation layer around the string-valued parts
/// of `ConversationItemKind`.  The generated contract deliberately leaves
/// these fields as strings for forward compatibility, so a comparison in a
/// view cannot get compiler help when the wire enum changes.
final class ConversationPresentationContractTests: XCTestCase {
    private struct Reference: CustomStringConvertible {
        let source: String
        let kind: String
        let field: String
        let value: String

        var description: String { "\(source): \(kind).\(field) == \(value)" }
    }

    func testPresentationStringEnumComparisonsAreDeclaredByTheCanonicalSchema() throws {
        let allowed = try Self.conversationEnumValues()
        let references = try Self.presentationReferences()
        XCTAssertFalse(references.isEmpty, "expected the conversation presentation to compare contract enums")

        for reference in references {
            let values = try XCTUnwrap(
                allowed[reference.kind]?[reference.field],
                "\(reference) does not name a string enum in conversation-item.schema.json"
            )
            XCTAssertTrue(
                values.contains(reference.value),
                "\(reference) is not allowed by schemas/remote-access/v2/conversation-item.schema.json; allowed: \(values.sorted().joined(separator: ", "))"
            )
        }
    }

    private static func conversationEnumValues() throws -> [String: [String: Set<String>]] {
        let data = try Data(contentsOf: repository.appendingPathComponent("schemas/remote-access/v2/conversation-item.schema.json"))
        let schema = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let properties = try XCTUnwrap(schema["properties"] as? [String: Any])
        let kind = try XCTUnwrap(properties["kind"] as? [String: Any])
        let variants = try XCTUnwrap(kind["oneOf"] as? [[String: Any]])

        var results: [String: [String: Set<String>]] = [:]
        for variant in variants {
            let variantProperties = try XCTUnwrap(variant["properties"] as? [String: Any])
            let type = try XCTUnwrap(variantProperties["type"] as? [String: Any])
            let kindName = try XCTUnwrap(type["const"] as? String)
            for (field, definition) in variantProperties {
                guard let definition = definition as? [String: Any],
                      let enumValues = definition["enum"] as? [String]
                else { continue }
                results[kindName, default: [:]][field] = Set(enumValues)
            }
        }
        return results
    }

    private static func presentationReferences() throws -> [Reference] {
        let files = try FileManager.default.contentsOfDirectory(
            at: app.appendingPathComponent("App/LatchMobile"),
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        )
        .filter { $0.pathExtension == "swift" }

        var results: [Reference] = []
        for file in files {
            let source = try String(contentsOf: file, encoding: .utf8)
            results += references(in: source, sourceName: file.lastPathComponent)
        }
        return results
    }

    /// Each binding is the field position in the generated enum case.  The
    /// allowed values themselves are never copied here; they come from schema.
    private static func references(in source: String, sourceName: String) -> [Reference] {
        let bindings: [(kind: String, field: String, pattern: String, capture: Int)] = [
            ("message", "role", #"case\s+\.message\s*\(\s*let\s+([A-Za-z_][A-Za-z0-9_]*)"#, 1),
            ("tool", "status", #"case\s+\.tool\s*\(\s*let\s+[A-Za-z_][A-Za-z0-9_]*\s*,\s*let\s+[A-Za-z_][A-Za-z0-9_]*\s*,\s*let\s+([A-Za-z_][A-Za-z0-9_]*)"#, 1),
            ("request", "requestType", #"case\s+\.request\s*\(\s*[^,]+\s*,\s*let\s+([A-Za-z_][A-Za-z0-9_]*)"#, 1),
            ("request", "status", #"case\s+\.request\s*\(.*?,\s*let\s+([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*:"#, 1),
        ]

        return bindings.flatMap { binding -> [Reference] in
            matches(binding.pattern, in: source).flatMap { match -> [Reference] in
                guard let variable = capture(binding.capture, from: match, in: source),
                      let body = caseBody(after: match.range, in: source)
                else { return [] }

                return stringComparisons(of: variable, in: body).map {
                    Reference(source: sourceName, kind: binding.kind, field: binding.field, value: $0)
                }
            }
        }
    }

    private static func caseBody(after range: NSRange, in source: String) -> String? {
        let tailRange = NSRange(location: range.location, length: (source as NSString).length - range.location)
        let nextCase = try? NSRegularExpression(pattern: #"\n\s*case\s+\."#)
        let end = nextCase?.firstMatch(in: source, range: tailRange)?.range.location ?? (source as NSString).length
        return (source as NSString).substring(with: NSRange(location: range.location, length: end - range.location))
    }

    private static func stringComparisons(of variable: String, in source: String) -> [String] {
        let escaped = NSRegularExpression.escapedPattern(for: variable)
        let pattern = #"(?:\b"# + escaped + #"\s*(?:==|!=)\s*\"([^\"]+)\"|\"([^\"]+)\"\s*(?:==|!=)\s*\b"# + escaped + #")"#
        guard let expression = try? NSRegularExpression(pattern: pattern) else { return [] }
        return expression.matches(in: source, range: NSRange(source.startIndex..., in: source)).compactMap {
            capture(1, from: $0, in: source) ?? capture(2, from: $0, in: source)
        }
    }

    private static func matches(_ pattern: String, in source: String) -> [NSTextCheckingResult] {
        guard let expression = try? NSRegularExpression(pattern: pattern, options: [.dotMatchesLineSeparators]) else { return [] }
        return expression.matches(in: source, range: NSRange(source.startIndex..., in: source))
    }

    private static func capture(_ index: Int, from match: NSTextCheckingResult, in source: String) -> String? {
        let range = match.range(at: index)
        guard range.location != NSNotFound else { return nil }
        return (source as NSString).substring(with: range)
    }

    private static let app: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // LatchMobileKitTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // LatchMobile

    private static let repository: URL = app
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // repository
}
