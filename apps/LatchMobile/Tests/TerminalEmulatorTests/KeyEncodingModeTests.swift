import XCTest

import LatchMobileKit
import SwiftTerm

/// The half of phase 5's verification that does not need a device.
///
/// `SwiftTermSurface.encode(_:)` builds a `TerminalKeyEncoder` from two flags
/// read off the live emulator — `applicationCursor` and `bracketedPasteMode` —
/// and the plan calls that the correctness trap of the whole feature: a bar
/// that hardcoded `ESC [ A` works in a shell and sends visible garbage into an
/// application that set DECCKM. The encoding table itself is unit-tested in the
/// kit against flags set by hand. What is left, and what this file measures, is
/// whether those flags actually track the traffic this surface will carry.
///
/// It is deliberately measured against recorded streams rather than reasoned
/// about, for the same reason the emulator was chosen that way.
///
/// This is the only test target that may depend on both the kit and SwiftTerm.
/// The rule the architecture draws is that `LatchMobileKit` must never *see*
/// the emulator; pairing them here is what makes the assertion mean anything,
/// because the question is precisely whether the emulator's flags and the kit's
/// table agree about a real Claude Code prompt.
final class KeyEncodingModeTests: XCTestCase {
    private static let esc: UInt8 = 0x1B

    /// The finding, and it is not what the plan assumed.
    ///
    /// Claude Code does **not** set DECCKM. Neither does Codex. Across the
    /// three recorded Claude streams and the Codex one, the private modes set
    /// are DECTCEM (`?25`), the alternate screen (`?1049`), mouse reporting
    /// (`?1000/1002/1003/1006`), focus reporting (`?1004`), bracketed paste
    /// (`?2004`), synchronised output (`?2026`) and unicode core (`?2031`) —
    /// and no `?1`. So in the case this feature exists for, the bar's arrows
    /// must send `ESC [ A`.
    ///
    /// Which is exactly why the indirection is load-bearing rather than
    /// hypothetical: hardcoding SS3 would have been wrong here, and hardcoding
    /// CSI would have been accidentally right here and wrong in `vim` and
    /// `less` — which the same phone reaches through the same bar.
    func testClaudeCodeLeavesCursorKeysInNormalMode() throws {
        for name in ["claude-code-startup", "claude-code-trust-prompt", "claude-code-turn"] {
            let terminal = try replay(named: name)
            XCTAssertFalse(
                terminal.applicationCursor,
                "\(name): Claude Code does not set DECCKM, so arrows are CSI here"
            )
            let encoder = encoder(for: terminal)
            XCTAssertEqual(encoder.encode(.up), [Self.esc, 0x5B, 0x41], "\(name): ESC [ A")
            XCTAssertEqual(encoder.encode(.down), [Self.esc, 0x5B, 0x42], "\(name): ESC [ B")
        }
    }

    /// Bracketed paste *is* on at a Claude Code prompt, which is the other
    /// mode the surface reads. A paste sent bare into it would arrive as
    /// keystrokes and submit at the first newline.
    func testClaudeCodePromptWantsBracketedPaste() throws {
        let terminal = try replay(named: "claude-code-trust-prompt")
        XCTAssertTrue(terminal.bracketedPasteMode, "the prompt sets ?2004h")
        XCTAssertEqual(
            encoder(for: terminal).pasteBytes("ls"),
            [Self.esc, 0x5B] + Array("200~".utf8) + Array("ls".utf8)
                + [Self.esc, 0x5B] + Array("201~".utf8)
        )
    }

    /// The other DECCKM state, from a recorded stream that does set it.
    ///
    /// `alternate-screen-enter-exit` is a full-screen application's entry and
    /// exit: `?1049h` with `?1h`, then both off again. Reading the flag mid
    /// stream rather than at the end is the whole point — the surface encodes
    /// at press time, and press time is while the application is up.
    func testApplicationCursorTracksARealApplicationsEntryAndExit() throws {
        let input = try fixtureBytes(named: "alternate-screen-enter-exit")
        let enter = try XCTUnwrap(range(of: [0x1B, 0x5B, 0x3F, 0x31, 0x68], in: input))
        let leave = try XCTUnwrap(range(of: [0x1B, 0x5B, 0x3F, 0x31, 0x6C], in: input))

        let terminal = makeTerminal(cols: 80, rows: 24)

        terminal.feed(buffer: input[0..<enter.upperBound])
        XCTAssertTrue(terminal.applicationCursor, "the application asked for SS3 arrows")
        XCTAssertEqual(encoder(for: terminal).encode(.up), [Self.esc, 0x4F, 0x41], "ESC O A")
        // Home and End shift with the arrows; the page keys never do.
        XCTAssertEqual(encoder(for: terminal).encode(.home), [Self.esc, 0x4F, 0x48])
        XCTAssertEqual(encoder(for: terminal).encode(.pageUp), [Self.esc, 0x5B] + Array("5~".utf8))

        terminal.feed(buffer: input[enter.upperBound..<leave.upperBound])
        XCTAssertFalse(terminal.applicationCursor, "the application gave the mode back on exit")
        XCTAssertEqual(encoder(for: terminal).encode(.up), [Self.esc, 0x5B, 0x41], "ESC [ A")
    }

    /// `reset()` is called between the preview still and the first live byte.
    /// A preview that had ended mid-application must not leave the bar
    /// encoding SS3 arrows into a shell.
    func testResetReturnsTheModesToTheirDefaults() {
        let terminal = makeTerminal(cols: 80, rows: 24)
        terminal.feed(text: "\u{1B}[?1h\u{1B}[?2004h")
        XCTAssertTrue(terminal.applicationCursor)
        XCTAssertTrue(terminal.bracketedPasteMode)

        terminal.resetToInitialState()

        XCTAssertFalse(terminal.applicationCursor)
        XCTAssertFalse(terminal.bracketedPasteMode)
        XCTAssertEqual(encoder(for: terminal).encode(.up), [Self.esc, 0x5B, 0x41])
    }

#if os(iOS)
    /// SwiftTerm's own control modifier, which the bar's sticky `ctrl` arms.
    ///
    /// The bar encodes its own keys, so this is not what `⌃C` goes through; it
    /// is what makes arming `ctrl` and then typing a letter on the *system*
    /// keyboard send a control chord, which is the stated reason `ctrl` is a
    /// modifier and not twenty-six more caps. It is spent on one keystroke and
    /// resets itself, which is what the bar listens for so an armed cap does
    /// not stay lit and read as locked.
    func testControlModifierIsSpentOnOneKeystrokeAndAnnouncesIt() {
        let view = TerminalView(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        let sent = SentBytes()
        let delegate = InputRecorder(sent: sent)
        view.terminalDelegate = delegate

        var resets = 0
        let token = NotificationCenter.default.addObserver(
            forName: .terminalViewControlModifierReset,
            object: view,
            queue: nil
        ) { _ in resets += 1 }
        defer { NotificationCenter.default.removeObserver(token) }

        view.controlModifier = true
        view.send(txt: "k")
        XCTAssertEqual(sent.bytes, [0x0B], "⌃K, not `k`")
        XCTAssertFalse(view.controlModifier, "spent on the one keystroke")
        XCTAssertEqual(resets, 1, "and it says so, so the cap can go out")

        sent.bytes = []
        view.send(txt: "k")
        XCTAssertEqual(sent.bytes, Array("k".utf8), "and the next letter is a letter")
    }
#endif

    // MARK: - Helpers

    /// Exactly what `SwiftTermSurface.encoder` does. Written out rather than
    /// shared because the app target is not importable from a test target, and
    /// a copy that drifts is the failure this test would then catch.
    private func encoder(for terminal: Terminal) -> TerminalKeyEncoder {
        TerminalKeyEncoder(
            cursorKeyApplicationMode: terminal.applicationCursor,
            bracketedPasteMode: terminal.bracketedPasteMode
        )
    }

    private func makeTerminal(cols: Int, rows: Int) -> Terminal {
        var options = TerminalOptions.default
        options.cols = cols
        options.rows = rows
        return Terminal(delegate: HeadlessDelegate(), options: options)
    }

    private func replay(named name: String) throws -> Terminal {
        let directory = EmulatorFixtureTests.fixtureRoot.appendingPathComponent(name)
        let meta = try EmulatorFixtureTests.json(at: directory.appendingPathComponent("meta.json"))
        let size = meta["size"] as! [String: Int]
        let terminal = makeTerminal(cols: size["cols"]!, rows: size["rows"]!)
        terminal.feed(byteArray: try fixtureBytes(named: name))
        return terminal
    }

    private func fixtureBytes(named name: String) throws -> [UInt8] {
        [UInt8](try Data(contentsOf: EmulatorFixtureTests.fixtureRoot
            .appendingPathComponent(name)
            .appendingPathComponent("input.bin")))
    }

    private func range(of needle: [UInt8], in haystack: [UInt8]) -> Range<Int>? {
        guard needle.count <= haystack.count else { return nil }
        for start in 0...(haystack.count - needle.count)
        where Array(haystack[start..<start + needle.count]) == needle {
            return start..<(start + needle.count)
        }
        return nil
    }
}

/// A delegate that answers nothing. The mode flags live on `Terminal` itself;
/// none of them arrive through a callback.
private final class HeadlessDelegate: TerminalDelegate {
    func send(source: Terminal, data: ArraySlice<UInt8>) {}
    func showCursor(source: Terminal) {}
    func hideCursor(source: Terminal) {}
    func setTerminalTitle(source: Terminal, title: String) {}
    func setTerminalIconTitle(source: Terminal, title: String) {}
    func sizeChanged(source: Terminal) {}
    func scrolled(source: Terminal, yDisp: Int) {}
    func linefeed(source: Terminal) {}
    func bufferActivated(source: Terminal) {}
    func bell(source: Terminal) {}
    func isProcessTrusted(source: Terminal) -> Bool { false }
    func mouseModeChanged(source: Terminal) {}
    func hostCurrentDirectoryUpdated(source: Terminal) {}
    func selectionChanged(source: Terminal) {}
}

#if os(iOS)
/// Somewhere for the bytes a `TerminalView` produces to land.
private final class SentBytes {
    var bytes: [UInt8] = []
}

private final class InputRecorder: NSObject, TerminalViewDelegate {
    private let sent: SentBytes

    init(sent: SentBytes) { self.sent = sent }

    func send(source: TerminalView, data: ArraySlice<UInt8>) {
        sent.bytes.append(contentsOf: data)
    }

    func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {}
    func setTerminalTitle(source: TerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
    func scrolled(source: TerminalView, position: Double) {}
    func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
    func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {}
}
#endif
