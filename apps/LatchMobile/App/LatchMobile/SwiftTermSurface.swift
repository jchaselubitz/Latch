#if canImport(UIKit)

import LatchMobileKit
import SwiftTerm
import SwiftUI
import UIKit

/// The real terminal behind `SessionTerminalSurface`.
///
/// **This file is the only place in Latch that may name SwiftTerm.**
/// `planning/PROJECT_ARCHITECTURE.md` requires the emulator to sit behind
/// Latch's own session-view API, and `docs/DECISION_MOBILE_TERMINAL_FALLBACK.md`
/// made that the load-bearing requirement rather than the library choice. Every
/// other file deals in bytes, `TerminalKey` values, and a grid size.
///
/// SwiftTerm was admitted by measurement, not by argument:
/// `Tests/TerminalEmulatorTests` replays all eleven recorded Claude Code and
/// Codex streams in `fixtures/vt/` and checks each against its `expected.json`.
/// That suite stays in the tree as the regression gate for upgrades.
///
/// SwiftTerm's own view type is spelled `SwiftTerm.TerminalView` throughout,
/// because Latch's terminal *screen* is also called `TerminalView` and the app
/// module's own type wins an unqualified lookup. Qualifying here rather than
/// renaming the screen keeps the emulator's name inside the one file allowed
/// to say it.
final class SwiftTermSurface: NSObject, SessionTerminalSurface {
    /// The view to put on screen. Owned here so that nothing above the seam
    /// has to hold a SwiftTerm type.
    let view: SwiftTerm.TerminalView

    var onInput: (ArraySlice<UInt8>) -> Void = { _ in }
    var onSizeChange: (Int, Int) -> Void = { _, _ in }
    var onControlModifierConsumed: () -> Void = {}

    /// The row of keys an iPhone keyboard does not have.
    ///
    /// SwiftTerm installs its own `TerminalAccessory` toolbar on iOS. It is
    /// replaced rather than augmented: Latch's bar emits `TerminalKey` values
    /// that this surface encodes against live modes, and running two accessory
    /// rows would mean two different answers to what `↑` sends. Assigning
    /// `nil` leaves the keyboard bare, which is the correct state while the
    /// session is only being previewed and there is nothing to type at.
    var accessoryView: UIView? {
        didSet { view.inputAccessoryView = accessoryView }
    }

    /// Puts Latch's key bar on the keyboard, in place of SwiftTerm's.
    ///
    /// The wiring is the whole point of the seam: the bar hands back a
    /// `TerminalKey`, this surface encodes it against the emulator's live
    /// modes, and the bytes go out the same path as a typed character. The bar
    /// never learns what `↑` sends.
    ///
    /// Phase 5 finishes this on a device — where the bar sits, how it travels
    /// with the keyboard, and how sticky `ctrl` and hold-to-repeat feel are
    /// all things a simulator answers unreliably.
    func installKeyBar() {
        let accessory = TerminalKeyBarAccessory(
            onKey: { [weak self] key in
                guard let self else { return }
                onInput(ArraySlice(encode(key)))
            },
            onDismiss: { [weak self] in self?.setFocus(false) }
        )
        // The bar's sticky `ctrl` and the emulator's own control modifier are
        // the same modifier seen from two sides. The bar encodes `⌃C` itself;
        // this is what makes arming `ctrl` and then typing `k` on the *system*
        // keyboard send `0x0B` rather than `k`, which is the whole reason
        // `ctrl` is a modifier instead of twenty-six more keys.
        accessory.state.onControlChange = { [weak self] armed in
            self?.setControlModifier(armed)
        }
        keyBar = accessory
        accessoryView = accessory
    }

    /// Takes the bar away. There is nothing to type at while a session is only
    /// being previewed, and a bar over a still would offer keys that go nowhere.
    func removeKeyBar() {
        keyBar?.state.setControl(.off)
        keyBar = nil
        accessoryView = nil
        setControlModifier(false)
    }

    private var keyBar: TerminalKeyBarAccessory?

    init(fontSize: CGFloat = 12) {
        view = SwiftTerm.TerminalView(frame: .zero, font: UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular))
        super.init()
        view.terminalDelegate = self
        // Suppress SwiftTerm's own accessory until Latch supplies one.
        // `TerminalView.setup()` has already installed a `TerminalAccessory`
        // and cleared `inputAssistantItem`'s button groups, so dropping it here
        // leaves the keyboard bare on iPhone and free of the iPad shortcut bar.
        view.inputAccessoryView = nil
        // SwiftTerm spends `controlModifier` on the next keystroke and posts
        // this when it does. Without it a bar cap armed for one letter stays
        // lit after that letter, which is what "locked" looks like.
        controlResetObserver = NotificationCenter.default.addObserver(
            forName: .terminalViewControlModifierReset,
            object: view,
            // Delivered synchronously on the posting thread, which is the main
            // thread the keystroke arrived on. A queued delivery would re-arm
            // a *locked* modifier one runloop turn late, and a fast typist
            // would lose the chord in between.
            queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            keyBar?.state.controlWasConsumed()
            onControlModifierConsumed()
        }
    }

    deinit {
        // A block-based observer is identified by its token, not by `self`.
        if let controlResetObserver {
            NotificationCenter.default.removeObserver(controlResetObserver)
        }
    }

    private var controlResetObserver: (any NSObjectProtocol)?

    // MARK: - SessionTerminalSurface

    func feed(_ bytes: Data) {
        view.feed(byteArray: ArraySlice(bytes))
    }

    /// Clears the grid, the scrollback and every mode.
    ///
    /// Phase 4 calls this between the preview still and the first live byte.
    /// The kernel repaints the pane's current frame on attach, and letting
    /// that land on top of a preview drawn at a different geometry would
    /// interleave two pictures of the same pane. A full reset also drops the
    /// preview's modes, which matters because a captured pane reproduces the
    /// grid but not the terminal state that produced it.
    func reset() {
        view.getTerminal().resetToInitialState()
    }

    /// Arms SwiftTerm's own control modifier for the next system-keyboard
    /// character. It is spent on that keystroke and resets itself; `locked`
    /// re-arms it from the reset notification.
    func setControlModifier(_ armed: Bool) {
        view.controlModifier = armed
    }

    func setFocus(_ focused: Bool) {
        if focused {
            _ = view.becomeFirstResponder()
        } else {
            _ = view.resignFirstResponder()
        }
    }

    /// Encodes a logical key against the emulator's *current* modes.
    ///
    /// This is the reason `encode` is on the seam at all. Arrows are `ESC [ A`
    /// normally and `ESC O A` once an application sets DECCKM, and every
    /// full-screen TUI this feature exists to reach sets it. Reading the flag
    /// off the live terminal at press time is what makes the same bar correct
    /// in a shell and inside Claude Code.
    func encode(_ key: TerminalKey) -> [UInt8] {
        encoder.encode(key)
    }

    func paste(_ text: String) {
        onInput(ArraySlice(encoder.pasteBytes(text)))
    }

    /// The encoding table, with its two flags read fresh from the emulator.
    /// The table itself lives in `LatchMobileKit` so the stub and this surface
    /// can only ever disagree about the modes, never about the sequences.
    private var encoder: TerminalKeyEncoder {
        let terminal = view.getTerminal()
        return TerminalKeyEncoder(
            cursorKeyApplicationMode: terminal.applicationCursor,
            bracketedPasteMode: terminal.bracketedPasteMode
        )
    }
}

// MARK: - TerminalViewDelegate

extension SwiftTermSurface: TerminalViewDelegate {
    /// Everything the user produced — typed characters, the key bar, a paste
    /// SwiftTerm handled itself — arrives here and goes straight to the pty.
    func send(source: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
        onInput(data)
    }

    /// The grid the view settled on after a layout or font change.
    ///
    /// Reporting it is not the same as declaring it: the geometry rule in the
    /// plan says only a deliberate grid change sends a resize frame, and
    /// `TerminalSession.resize` is what decides. The keyboard appearing must
    /// reach the pty as nothing at all.
    func sizeChanged(source: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
        onSizeChange(newCols, newRows)
    }

    func setTerminalTitle(source: SwiftTerm.TerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: SwiftTerm.TerminalView, directory: String?) {}
    func scrolled(source: SwiftTerm.TerminalView, position: Double) {}
    func rangeChanged(source: SwiftTerm.TerminalView, startY: Int, endY: Int) {}

    /// Links are not opened from the terminal surface. A session's output is
    /// whatever the agent printed, and a tap that left the app for an
    /// arbitrary URL would be a navigation the user did not ask for.
    func requestOpenLink(source: SwiftTerm.TerminalView, link: String, params: [String: String]) {}
}

// MARK: - SwiftUI

/// Puts the surface's view on screen.
///
/// The surface is created and owned by the caller rather than by
/// `makeUIView`, because it outlives any single layout pass: bytes are fed
/// into it from `TerminalSession.output` while SwiftUI is free to rebuild the
/// view tree around it.
struct SwiftTermSurfaceView: UIViewRepresentable {
    let surface: SwiftTermSurface
    /// Point size for the grid the caller chose. The geometry rule derives it
    /// from cols and the viewport width — font size follows from the grid, not
    /// the other way round.
    var fontSize: CGFloat

    func makeUIView(context: Context) -> SwiftTerm.TerminalView {
        surface.view
    }

    func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {
        if view.font.pointSize != fontSize {
            view.font = UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
        }
    }
}

#endif
