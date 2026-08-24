import XCTest

@testable import LatchMobileKit

/// A surface that owns the modes, the way a real emulator-backed one does.
///
/// The bar never sees these flags; it presses `.up` and gets whatever the
/// terminal is currently in the mood for. Testing through this fake rather
/// than through `TerminalKeyEncoder` directly is the point: it asserts the
/// *seam* resolves the mode at press time, not that the table is right.
private final class FakeTerminalSurface: TerminalKeyEncoding {
    var encoder = TerminalKeyEncoder()
    private(set) var sent: [[UInt8]] = []

    func encode(_ key: TerminalKey) -> [UInt8] { encoder.encode(key) }

    func paste(_ text: String) { sent.append(encoder.pasteBytes(text)) }
}

final class TerminalKeyTests: XCTestCase {
    private let encoder = TerminalKeyEncoder()

    // MARK: - The unambiguous ones

    func testFixedEncodings() {
        XCTAssertEqual(encoder.encode(.escape), [0x1B])
        XCTAssertEqual(encoder.encode(.tab), [0x09])
        XCTAssertEqual(encoder.encode(.backspace), [0x7F])
        XCTAssertEqual(encoder.encode(.delete), Array("\u{1B}[3~".utf8))
        XCTAssertEqual(encoder.encode(.backTab), Array("\u{1B}[Z".utf8))
        XCTAssertEqual(encoder.encode(.pageUp), Array("\u{1B}[5~".utf8))
        XCTAssertEqual(encoder.encode(.pageDown), Array("\u{1B}[6~".utf8))
    }

    func testControlChordsFoldIntoTheC0Range() {
        XCTAssertEqual(encoder.encode(.control("c")), [0x03])
        XCTAssertEqual(encoder.encode(.control("C")), [0x03])
        XCTAssertEqual(encoder.encode(.control("d")), [0x04])
        XCTAssertEqual(encoder.encode(.control("l")), [0x0C])
        XCTAssertEqual(encoder.encode(.control("r")), [0x12])
        XCTAssertEqual(encoder.encode(.control("z")), [0x1A])
        // The two that do not follow the mask.
        XCTAssertEqual(encoder.encode(.control(" ")), [0x00])
        XCTAssertEqual(encoder.encode(.control("?")), [0x7F])
    }

    func testLiteralsAreUTF8() {
        XCTAssertEqual(encoder.encode(.literal("|")), [0x7C])
        XCTAssertEqual(encoder.encode(.literal("~")), [0x7E])
    }

    func testFunctionKeys() {
        XCTAssertEqual(encoder.encode(.function(1)), Array("\u{1B}OP".utf8))
        XCTAssertEqual(encoder.encode(.function(4)), Array("\u{1B}OS".utf8))
        XCTAssertEqual(encoder.encode(.function(5)), Array("\u{1B}[15~".utf8))
        // The gap at 16 is why F6 is 17 and not 16.
        XCTAssertEqual(encoder.encode(.function(6)), Array("\u{1B}[17~".utf8))
        XCTAssertEqual(encoder.encode(.function(12)), Array("\u{1B}[24~".utf8))
    }

    // MARK: - The ones that depend on a mode the bar cannot see

    func testArrowsInNormalCursorMode() {
        let surface = FakeTerminalSurface()
        surface.encoder.cursorKeyApplicationMode = false

        XCTAssertEqual(surface.encode(.up), Array("\u{1B}[A".utf8))
        XCTAssertEqual(surface.encode(.down), Array("\u{1B}[B".utf8))
        XCTAssertEqual(surface.encode(.right), Array("\u{1B}[C".utf8))
        XCTAssertEqual(surface.encode(.left), Array("\u{1B}[D".utf8))
        XCTAssertEqual(surface.encode(.home), Array("\u{1B}[H".utf8))
        XCTAssertEqual(surface.encode(.end), Array("\u{1B}[F".utf8))
    }

    /// This is the case the whole indirection exists for: a full-screen agent
    /// TUI has set DECCKM, and a bar that hardcoded `ESC [ A` would be typing
    /// literal `[A` into the prompt.
    func testArrowsUnderDECCKM() {
        let surface = FakeTerminalSurface()
        surface.encoder.cursorKeyApplicationMode = true

        XCTAssertEqual(surface.encode(.up), Array("\u{1B}OA".utf8))
        XCTAssertEqual(surface.encode(.down), Array("\u{1B}OB".utf8))
        XCTAssertEqual(surface.encode(.right), Array("\u{1B}OC".utf8))
        XCTAssertEqual(surface.encode(.left), Array("\u{1B}OD".utf8))
        XCTAssertEqual(surface.encode(.home), Array("\u{1B}OH".utf8))
        XCTAssertEqual(surface.encode(.end), Array("\u{1B}OF".utf8))
    }

    /// Page keys look like arrows on a keyboard and are not arrows on the
    /// wire: DECCKM must not move them.
    func testPageKeysIgnoreCursorMode() {
        let surface = FakeTerminalSurface()
        surface.encoder.cursorKeyApplicationMode = true

        XCTAssertEqual(surface.encode(.pageUp), Array("\u{1B}[5~".utf8))
        XCTAssertEqual(surface.encode(.pageDown), Array("\u{1B}[6~".utf8))
        XCTAssertEqual(surface.encode(.escape), [0x1B])
    }

    // MARK: - Paste

    func testPasteIsBareUnlessBracketedPasteIsOn() {
        let surface = FakeTerminalSurface()
        surface.paste("ls -al")
        XCTAssertEqual(surface.sent, [Array("ls -al".utf8)])
    }

    func testPasteIsWrappedWhenTheApplicationAskedForIt() {
        let surface = FakeTerminalSurface()
        surface.encoder.bracketedPasteMode = true
        surface.paste("ls -al")
        XCTAssertEqual(surface.sent, [Array("\u{1B}[200~ls -al\u{1B}[201~".utf8)])
    }
}

extension TerminalKeyTests {
    func testStickyControlOnlyModifiesSingleCharacterLiterals() {
        XCTAssertEqual(TerminalKey.literal("c").applyingControl(), .control("c"))
        XCTAssertEqual(TerminalKey.literal("|").applyingControl(), .control("|"))
        // Nothing to fold: these pass through, and the bar still disarms.
        XCTAssertEqual(TerminalKey.up.applyingControl(), .up)
        XCTAssertEqual(TerminalKey.escape.applyingControl(), .escape)
        XCTAssertEqual(TerminalKey.literal("ls").applyingControl(), .literal("ls"))
    }
}
