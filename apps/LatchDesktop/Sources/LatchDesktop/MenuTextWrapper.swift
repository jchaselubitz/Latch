import AppKit
import Foundation

/// Hard-wraps text destined for an `NSMenu` item.
///
/// The menu bar extra renders as a native menu, and native menu items never
/// wrap on their own: a long session description would stretch the submenu to
/// the width of the screen. Breaking the string into newline-separated lines up
/// front keeps the item inside `maxWidth` while AppKit still draws it as one
/// multi-line item.
enum MenuTextWrapper {
    /// Submenu header width cap requested by the design.
    static let defaultMaxWidth: CGFloat = 250

    static func wrap(_ text: String, maxWidth: CGFloat = defaultMaxWidth) -> String {
        let font = NSFont.menuFont(ofSize: 0)
        return wrap(text, maxWidth: maxWidth) { candidate in
            (candidate as NSString).size(withAttributes: [.font: font]).width
        }
    }

    /// Greedy word wrap. `measure` reports the drawn width of a candidate line;
    /// injecting it keeps the algorithm testable without a font.
    static func wrap(
        _ text: String,
        maxWidth: CGFloat,
        measure: (String) -> CGFloat
    ) -> String {
        let words = text.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        guard !words.isEmpty else { return "" }

        var lines: [String] = []
        var current = ""
        for word in words {
            let candidate = current.isEmpty ? word : current + " " + word
            if measure(candidate) <= maxWidth {
                current = candidate
                continue
            }
            if !current.isEmpty {
                lines.append(current)
                current = ""
            }
            // A single word can still overflow (paths in the fallback subtitle
            // have no spaces to break on), so split it across lines.
            if measure(word) <= maxWidth {
                current = word
            } else {
                var chunk = ""
                for character in word {
                    if !chunk.isEmpty, measure(chunk + String(character)) > maxWidth {
                        lines.append(chunk)
                        chunk = ""
                    }
                    chunk.append(character)
                }
                current = chunk
            }
        }
        if !current.isEmpty { lines.append(current) }
        return lines.joined(separator: "\n")
    }
}
