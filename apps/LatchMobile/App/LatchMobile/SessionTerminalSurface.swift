import LatchMobileKit
import SwiftUI

/// Latch's own terminal-view API. Everything above this line deals in bytes,
/// logical keys, and a grid size; nothing above it may name an emulator.
///
/// `planning/PROJECT_ARCHITECTURE.md` requires the emulator to sit behind this
/// seam, and the decision record made that the load-bearing requirement rather
/// than the library choice. Phase 3′ fills it with `StubTerminalSurface` so the
/// key bar can be built and pressed while the renderer is still being measured
/// against `fixtures/vt/`; phase 3 adds `SwiftTermSurface` beside it.
protocol SessionTerminalSurface: AnyObject, TerminalKeyEncoding {
    /// Bytes from the kernel, in arrival order.
    func feed(_ bytes: Data)

    /// Clears the grid and scrollback.
    ///
    /// Phase 4 calls this between the preview still and the first live byte:
    /// the kernel repaints the pane's current frame on attach, and letting that
    /// land on top of a preview drawn at a different geometry would interleave
    /// two pictures of the same pane.
    func reset()

    /// Raises or resigns first responder, which is also what shows and hides
    /// the key bar — the bar is the surface's `inputAccessoryView`.
    func setFocus(_ focused: Bool)

    /// Arms or disarms the control modifier for the next character typed on
    /// the *system* keyboard.
    ///
    /// The bar encodes its own keys itself — `⌃C` is one press — so this exists
    /// only for the letters the bar does not carry. Arming `ctrl` and typing
    /// `k` has to send `0x0B`, and the plan's stated reason for `ctrl` being a
    /// sticky modifier rather than a key ("the difference between `⌃C` being
    /// one key and the bar needing a `⌃` variant of every letter") is precisely
    /// that case. The emulator owns it because the emulator is what receives
    /// the keystroke.
    func setControlModifier(_ armed: Bool)

    /// Called when the surface spent the armed modifier on a keystroke.
    ///
    /// A sticky modifier that is not told when it was used stays lit after the
    /// key it modified, which reads as locked. Locking is a separate, explicit
    /// gesture on the bar, so the two must not be confusable.
    var onControlModifierConsumed: () -> Void { get set }

    /// Bytes the user produced, headed for the pty.
    var onInput: (ArraySlice<UInt8>) -> Void { get set }

    /// The grid the surface settled on, in columns and rows.
    var onSizeChange: (Int, Int) -> Void { get set }
}

/// A surface with no emulator behind it: incoming bytes are rendered as text,
/// and anything typed is echoed back into that text.
///
/// It exists so the key bar is a testable, pressable thing before a dependency
/// is committed. Its two mode flags are settable by hand precisely because the
/// interesting question — does `↑` send `ESC [ A` or `ESC O A`? — is a question
/// about modes, and a stub that could not be put into DECCKM could not ask it.
@Observable
final class StubTerminalSurface: SessionTerminalSurface {
    /// Everything fed or echoed so far, control bytes made visible.
    private(set) var text = ""

    /// The modes a real emulator would read off its own parser state.
    @ObservationIgnored var encoder = TerminalKeyEncoder()
    @ObservationIgnored private(set) var focused = false

    @ObservationIgnored var onInput: (ArraySlice<UInt8>) -> Void = { _ in }
    @ObservationIgnored var onSizeChange: (Int, Int) -> Void = { _, _ in }
    @ObservationIgnored var onControlModifierConsumed: () -> Void = {}

    /// What a real surface hands to the emulator. A stub has no keyboard to
    /// apply it to, so it only records it — which is enough to assert that the
    /// bar arms and disarms it at the right moments.
    @ObservationIgnored private(set) var controlModifierArmed = false

    /// A fixed grid: there is no layout to measure without a renderer.
    let cols: Int
    let rows: Int

    init(cols: Int = 80, rows: Int = 24) {
        self.cols = cols
        self.rows = rows
        // Local echo, because there is no kernel behind a stub. This is the
        // whole affordance: press `esc` on the bar and `^[` appears.
        onInput = { [weak self] bytes in
            self?.append(Array(bytes))
        }
    }

    func feed(_ bytes: Data) { append(Array(bytes)) }

    func reset() { text = "" }

    func setFocus(_ focused: Bool) { self.focused = focused }

    func setControlModifier(_ armed: Bool) { controlModifierArmed = armed }

    func encode(_ key: TerminalKey) -> [UInt8] { encoder.encode(key) }

    func paste(_ text: String) { onInput(ArraySlice(encoder.pasteBytes(text))) }

    /// Renders bytes the way a terminal transcript does — printable characters
    /// as themselves, control bytes in caret notation — so what the bar sent is
    /// legible rather than invisible.
    private func append(_ bytes: [UInt8]) {
        for byte in bytes {
            switch byte {
            case 0x20...0x7E: text.append(Character(UnicodeScalar(byte)))
            case 0x0A: text.append("\n")
            case 0x7F: text.append("^?")
            case 0x00...0x1F: text.append("^" + String(UnicodeScalar(byte + 0x40)))
            default: text.append(Character(UnicodeScalar(byte)))
            }
        }
    }
}

/// The stub's rendering: fed and echoed bytes as monospaced text, pinned to
/// the bottom the way a terminal is. It is a debugging window onto the key
/// bar, not a terminal, and it goes away when `SwiftTermSurface` arrives.
struct StubTerminalSurfaceView: View {
    let surface: StubTerminalSurface

    var body: some View {
        ScrollView {
            Text(surface.text)
                .font(.system(size: 12, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
        }
        .defaultScrollAnchor(.bottom)
        .background(Color.black.opacity(0.9))
    }
}
