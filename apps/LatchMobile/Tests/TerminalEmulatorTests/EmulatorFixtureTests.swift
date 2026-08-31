import XCTest

import SwiftTerm

/// The measurement gate from `docs/PLAN_MOBILE_TERMINAL_VIEW.md` phase 3.
///
/// `docs/DECISION_EMULATOR.md` set the precedent: an emulator is chosen by
/// being fed every case in `fixtures/vt/` and checked against each fixture's
/// hand-authored `expected.json`, not by being compared on documentation. The
/// eleven fixtures are recorded Claude Code and Codex PTY streams — exactly the
/// traffic the mobile terminal surface will carry — so this is the same
/// measurement applied to the phone's renderer.
///
/// It stays in the tree after the choice is made. The gate that admitted the
/// dependency is the regression test that keeps it honest across upgrades.
///
/// This target exists separately from `LatchMobileKitTests` for one reason:
/// the kit must never see the emulator. Only this target and the app's
/// `SwiftTermSurface` may name SwiftTerm.
final class EmulatorFixtureTests: XCTestCase {
    func testEveryFixtureReplaysCorrectly() throws {
        let fixtures = try Self.fixtureNames()
        XCTAssertEqual(fixtures.count, 11, "the recorded suite is eleven cases")

        for name in fixtures {
            try replay(named: name)
        }
    }

    // MARK: - one fixture

    private func replay(named name: String) throws {
        let directory = Self.fixtureRoot.appendingPathComponent(name)
        let meta = try Self.json(at: directory.appendingPathComponent("meta.json"))
        let expected = try Self.json(at: directory.appendingPathComponent("expected.json"))
        let input = [UInt8](try Data(contentsOf: directory.appendingPathComponent("input.bin")))

        let startSize = meta["size"] as! [String: Int]
        let recorder = FixtureDelegate()
        var options = TerminalOptions.default
        options.cols = startSize["cols"]!
        options.rows = startSize["rows"]!
        // The Mac kernel keeps bounded history; the fixtures only need
        // enough for `high-rate-output` to have dropped its head.
        options.scrollback = 5000
        let terminal = Terminal(delegate: recorder, options: options)

        // Feed in recorded order, applying each resize at the byte offset the
        // pty delivered it at. A resize applied at the end instead would test a
        // different thing entirely: `claude-code-resize-alt-screen` exists to
        // check that the application redraws *after* being resized mid-paint.
        var cursor = 0
        for resize in (meta["resizes"] as? [[String: Any]]) ?? [] {
            let at = resize["at_byte"] as! Int
            if at > cursor {
                terminal.feed(buffer: input[cursor..<at])
                cursor = at
            }
            terminal.resize(cols: resize["cols"] as! Int, rows: resize["rows"] as! Int)
        }
        if cursor < input.count {
            terminal.feed(buffer: input[cursor...])
        }

        let where_ = "fixture \(name)"

        let wantSize = expected["size"] as! [String: Int]
        XCTAssertEqual(terminal.cols, wantSize["cols"]!, "\(where_): cols")
        XCTAssertEqual(terminal.rows, wantSize["rows"]!, "\(where_): rows")

        XCTAssertEqual(
            terminal.isCurrentBufferAlternate,
            expected["alternate_screen"] as! Bool,
            "\(where_): alternate screen"
        )

        let cursorExpectation = expected["cursor"] as? [String: Any] ?? [:]
        if let visible = cursorExpectation["visible"] as? Bool {
            // Read off the delegate rather than the terminal: DECTCEM arrives
            // as show/hide callbacks, which is also how the renderer learns it.
            XCTAssertEqual(recorder.cursorVisible, visible, "\(where_): cursor visibility")
        }
        if let row = cursorExpectation["row"] as? Int, let col = cursorExpectation["col"] as? Int {
            XCTAssertEqual(terminal.buffer.y, row, "\(where_): cursor row")
            XCTAssertEqual(terminal.buffer.x, col, "\(where_): cursor column")
        }

        if let region = expected["scroll_region"] as? [String: Int] {
            XCTAssertEqual(terminal.buffer.scrollTop, region["top"]!, "\(where_): scroll top")
            XCTAssertEqual(terminal.buffer.scrollBottom, region["bottom"]!, "\(where_): scroll bottom")
        }

        if let needles = expected["contains"] as? [String] {
            let screen = Self.screenText(of: terminal)
            for needle in needles {
                XCTAssertTrue(
                    screen.contains(needle),
                    "\(where_): screen does not contain \(needle.debugDescription)"
                )
            }
        }

        if let rows = expected["rows"] as? [String: String] {
            for (index, text) in rows {
                XCTAssertEqual(
                    Self.text(of: terminal, row: Int(index)!),
                    text,
                    "\(where_): row \(index)"
                )
            }
        }

        if let modes = expected["modes"] as? [String: Any] {
            if let want = modes["bracketed_paste"] as? Bool {
                XCTAssertEqual(terminal.bracketedPasteMode, want, "\(where_): bracketed paste")
            }
            if let want = modes["application_cursor_keys"] as? Bool {
                // The mode the key bar's arrows depend on. A fixture that
                // records it is a fixture that pins `TerminalKey` encoding
                // against real application traffic.
                XCTAssertEqual(terminal.applicationCursor, want, "\(where_): DECCKM")
            }
        }
    }

    // MARK: - reading the grid

    /// The extraction SwiftTerm uses for its own copy path, and the only
    /// correct one.
    ///
    /// `BufferLine.translateToString()` on its own reads a painted screen
    /// wrong twice over: a cell that was never written holds code 0, which
    /// comes back as a NUL rather than a blank, and an extended grapheme
    /// cluster (a combining mark, a ZWJ sequence) lives in the terminal's
    /// character map and comes back as a placeholder. Both are fixed by going
    /// through `terminal.getCharacter(for:)` and mapping NUL to space.
    ///
    /// Any Latch code that reads text out of a SwiftTerm grid — a preview
    /// diff, an accessibility label, a copy action — has to use this rule.
    static func text(of terminal: Terminal, row: Int) -> String {
        guard let line = terminal.getLine(row: row) else { return "" }
        return line.translateToString(
            trimRight: true,
            skipNullCellsFollowingWide: true,
            characterProvider: { terminal.getCharacter(for: $0) }
        ).replacingOccurrences(of: "\u{0}", with: " ")
    }

    static func screenText(of terminal: Terminal) -> String {
        (0..<terminal.rows).map { text(of: terminal, row: $0) }.joined(separator: "\n")
    }

    // MARK: - locating the fixtures

    /// `fixtures/vt` is repository-level and shared with the Rust kernel's own
    /// replay tests, so it is reached by path rather than copied in as a test
    /// resource. Copying would let the two suites drift onto different
    /// recordings of the same stream.
    static let fixtureRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()   // TerminalEmulatorTests
        .deletingLastPathComponent()   // Tests
        .deletingLastPathComponent()   // LatchMobile
        .deletingLastPathComponent()   // apps
        .deletingLastPathComponent()   // <repo root>
        .appendingPathComponent("fixtures/vt")

    static func fixtureNames() throws -> [String] {
        try FileManager.default
            .contentsOfDirectory(atPath: fixtureRoot.path)
            .filter { !$0.hasPrefix(".") }
            .sorted()
    }

    static func json(at url: URL) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as! [String: Any]
    }
}

/// Records the two things a delegate is the only route to: cursor visibility,
/// which arrives as DECTCEM callbacks, and the window title.
private final class FixtureDelegate: TerminalDelegate {
    var cursorVisible = true

    func showCursor(source: Terminal) { cursorVisible = true }
    func hideCursor(source: Terminal) { cursorVisible = false }
    func send(source: Terminal, data: ArraySlice<UInt8>) {}
}
