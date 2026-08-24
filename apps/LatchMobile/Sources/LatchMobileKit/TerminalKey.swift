import Foundation

/// A key the accessory bar can press, named by what it *means* rather than by
/// what it sends.
///
/// The bar deals in these and never in escape sequences. That is not tidiness:
/// arrows are `ESC [ A` in normal mode and `ESC O A` once an application sets
/// DECCKM, and every full-screen TUI this feature exists to reach — Claude
/// Code, Codex, vim, less — sets it. A bar that hardcoded `ESC [ A` would work
/// in a shell and send visible garbage into an agent prompt. Only the surface
/// knows which mode the emulator is in, so only the surface may encode.
public enum TerminalKey: Equatable, Sendable {
    case escape, tab, backTab, backspace, delete
    case up, down, left, right
    case home, end, pageUp, pageDown
    case function(Int)
    /// A control chord: `⌃C`, `⌃D`, `⌃Z`, `⌃R`, `⌃L`.
    case control(Character)
    /// A character the soft keyboard makes awkward: `|` `~` `/` `-` `_` `` ` ``
    /// `{` `}` `[` `]` `<` `>` `$` `&` `*`.
    case literal(String)

    /// What this key becomes when the bar's sticky `ctrl` is armed.
    ///
    /// Only a single-character literal has a control chord; `⌃` plus an arrow
    /// or `⌃` plus `esc` are real sequences on a hardware keyboard but have no
    /// place on a bar whose whole job is the keys the soft keyboard lacks, and
    /// silently inventing one would send bytes the user did not ask for. Those
    /// pass through unmodified, and the bar disarms either way.
    public func applyingControl() -> TerminalKey {
        guard case .literal(let text) = self, text.count == 1, let character = text.first
        else { return self }
        return .control(character)
    }
}

/// Anything that can turn a `TerminalKey` into bytes for the pty.
///
/// This lives in the kit rather than beside the surface protocol in the app so
/// that the mode-dependent encodings can be tested by `swift test` with no
/// simulator and no emulator present. `SessionTerminalSurface` refines it.
public protocol TerminalKeyEncoding: AnyObject {
    /// Encodes a logical key using the terminal's *current* modes.
    func encode(_ key: TerminalKey) -> [UInt8]
    /// Sends pasted text, wrapped in bracketed-paste markers only if the
    /// application turned that mode on.
    func paste(_ text: String)
}

/// The encoding rules themselves, as a value.
///
/// A real surface holds one of these with its two flags read from the
/// emulator's live state; a stub holds one with the flags set by hand. Keeping
/// the table in one place means the stub and the emulator-backed surface can
/// only disagree about the *modes*, never about the sequences.
public struct TerminalKeyEncoder: Equatable, Sendable {
    /// DECCKM (`ESC [ ? 1 h`). Application cursor keys.
    public var cursorKeyApplicationMode: Bool
    /// Bracketed paste (`ESC [ ? 2004 h`).
    public var bracketedPasteMode: Bool

    public init(cursorKeyApplicationMode: Bool = false, bracketedPasteMode: Bool = false) {
        self.cursorKeyApplicationMode = cursorKeyApplicationMode
        self.bracketedPasteMode = bracketedPasteMode
    }

    private static let esc: UInt8 = 0x1B

    public func encode(_ key: TerminalKey) -> [UInt8] {
        switch key {
        case .escape: return [Self.esc]
        case .tab: return [0x09]
        // Shift-Tab has no application-mode variant; it is CSI Z in both.
        case .backTab: return csi("Z")
        // DEL, not BS: this is what a Mac's delete key sends, and what every
        // readline and TUI on the other end expects to mean "erase left".
        case .backspace: return [0x7F]
        case .delete: return csi("3~")

        // The four that make this enum necessary.
        case .up: return cursor("A")
        case .down: return cursor("B")
        case .right: return cursor("C")
        case .left: return cursor("D")
        // Home and End shift the same way as the arrows.
        case .home: return cursor("H")
        case .end: return cursor("F")

        // Page keys are CSI ~ sequences and do not follow DECCKM.
        case .pageUp: return csi("5~")
        case .pageDown: return csi("6~")

        case .function(let n): return function(n)
        case .control(let character): return control(character)
        case .literal(let text): return Array(text.utf8)
        }
    }

    /// The bytes for a paste, bracketed only when the application asked to be
    /// told where a paste begins and ends.
    public func pasteBytes(_ text: String) -> [UInt8] {
        let body = Array(text.utf8)
        guard bracketedPasteMode else { return body }
        return csi("200~") + body + csi("201~")
    }

    private func csi(_ tail: String) -> [UInt8] {
        [Self.esc, 0x5B] + Array(tail.utf8)
    }

    /// `ESC [ x` normally, `ESC O x` under DECCKM.
    private func cursor(_ final: String) -> [UInt8] {
        cursorKeyApplicationMode
            ? [Self.esc, 0x4F] + Array(final.utf8)
            : csi(final)
    }

    /// F1–F4 are SS3 sequences in both modes; F5 and up are CSI `~` codes with
    /// the historical gaps at 16 and 22.
    private func function(_ n: Int) -> [UInt8] {
        switch n {
        case 1: return [Self.esc, 0x4F, 0x50]
        case 2: return [Self.esc, 0x4F, 0x51]
        case 3: return [Self.esc, 0x4F, 0x52]
        case 4: return [Self.esc, 0x4F, 0x53]
        case 5: return csi("15~")
        case 6...10: return csi("\(n + 11)~")
        case 11, 12: return csi("\(n + 12)~")
        default: return []
        }
    }

    /// `⌃` folds the character into the C0 range. This covers the letters and
    /// also `@ [ \ ] ^ _`, which land on 0x00 and 0x1B–0x1F by the same rule.
    private func control(_ character: Character) -> [UInt8] {
        if character == " " || character == "@" { return [0x00] }
        // ⌃? is the one that does not follow the mask.
        if character == "?" { return [0x7F] }
        guard let ascii = character.uppercased().unicodeScalars.first?.value,
              ascii < 128
        else { return Array(String(character).utf8) }
        return [UInt8(ascii) & 0x1F]
    }
}
