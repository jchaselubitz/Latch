import LatchMobileKit
import SwiftUI
import UIKit

/// The keys an iPhone keyboard does not have.
///
/// Escape, Control, Tab and the arrows are exactly what a directory-trust
/// prompt, a permission modal and a stopped composer are answered with, so
/// without this row the terminal view does not do the thing it was added for.
///
/// It emits `TerminalKey` values and never bytes. See `TerminalKey` for why
/// that indirection is load-bearing rather than tidy.
/// The sticky modifier's state, and the one place that decides what "armed"
/// means to the surface.
///
/// It is a reference type on purpose. Two things change it: the `ctrl` cap,
/// and the surface — which spends the modifier the moment a letter is typed on
/// the system keyboard and says so. A `@State` flag inside the view could only
/// hear the first of those.
@Observable
final class TerminalKeyBarState {
    enum ControlState {
        case off, armed, locked

        var isOn: Bool { self != .off }
    }

    private(set) var control: ControlState = .off

    /// Told when the modifier arms or disarms, so the surface can arm its own
    /// for the next system-keyboard character.
    @ObservationIgnored var onControlChange: (Bool) -> Void = { _ in }

    func setControl(_ next: ControlState) {
        guard next != control else { return }
        control = next
        onControlChange(next.isOn)
    }

    /// The surface spent the modifier on a keystroke.
    ///
    /// Armed goes out; locked stays and re-arms the surface, which is the
    /// difference the long-press bought.
    func controlWasConsumed() {
        switch control {
        case .off: break
        case .armed: setControl(.off)
        case .locked: onControlChange(true)
        }
    }
}

struct TerminalKeyBar: View {
    /// A logical key press, already control-modified if the sticky modifier
    /// was armed. The caller encodes it through the surface.
    let onKey: (TerminalKey) -> Void
    let onDismiss: () -> Void
    /// The sticky modifier's state, held outside the view.
    ///
    /// It lives in a reference type because the *surface* also resets it: a
    /// letter typed on the system keyboard while `ctrl` is armed spends the
    /// modifier, and a cap still lit after that reads as locked.
    let state: TerminalKeyBarState

    /// The space budget, as numbers rather than as an intention. One row is
    /// the whole allowance: a second would be a third of the visible terminal
    /// on a small phone.
    private enum Metrics {
        static let barHeight: CGFloat = 34
        static let keyHeight: CGFloat = 28
        static let keyPadding: CGFloat = 10
        static let spacing: CGFloat = 6
        static let font = Font.system(size: 13, weight: .medium, design: .monospaced)
    }

    var body: some View {
        HStack(spacing: 0) {
            // The whole row scrolls, with no leading pinned group: pinning both
            // ends would cost ~90 pt of scrollable width to save one swipe.
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Metrics.spacing) {
                    // `esc  ctrl  tab  ← ↓ ↑ →  ⌃C  …`. `ctrl` is built
                    // separately from the rest because it is a modifier with
                    // its own state, not an entry in the key table.
                    cap(Self.escape)
                    controlCap
                    ForEach(Self.keys) { entry in
                        cap(entry)
                    }
                }
                .padding(.horizontal, Metrics.spacing)
            }

            // Pinned to the trailing edge so it never scrolls away: a user
            // reading output gets the whole screen back, and a tap on the
            // terminal brings the keyboard and this row back together.
            Divider()
            Button(action: onDismiss) {
                Image(systemName: "keyboard.chevron.compact.down")
                    .font(.system(size: 15, weight: .medium))
                    .frame(width: 40, height: Metrics.barHeight)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Hide keyboard")
        }
        .frame(height: Metrics.barHeight)
        .background(.bar)
    }

    private func cap(_ entry: Entry) -> some View {
        KeyCap(
            glyph: entry.glyph,
            font: Metrics.font,
            height: Metrics.keyHeight,
            padding: Metrics.keyPadding,
            isOn: false,
            repeats: entry.repeats,
            action: { press(entry.key) }
        )
    }

    // MARK: - The sticky modifier

    /// `ctrl` is a modifier rather than a key so the bar needs one `⌃` and not
    /// a control variant of every letter. Tap arms it for the next press;
    /// long-press locks it until tapped again.
    private var controlCap: some View {
        KeyCap(
            glyph: "ctrl",
            font: Metrics.font,
            height: Metrics.keyHeight,
            padding: Metrics.keyPadding,
            isOn: state.control.isOn,
            repeats: false,
            longPressDuration: 0.5,
            action: { state.setControl(state.control.isOn ? .off : .armed) },
            onLongPress: { state.setControl(state.control == .locked ? .off : .locked) }
        )
        .accessibilityLabel("Control")
        .accessibilityValue(accessibilityValueForControl)
    }

    private var accessibilityValueForControl: String {
        switch state.control {
        case .off: "off"
        case .armed: "armed for the next key"
        case .locked: "locked"
        }
    }

    private func press(_ key: TerminalKey) {
        // The bar encodes its own keys; the surface's modifier is for the
        // system keyboard's letters, and both must not fire on one press.
        onKey(state.control.isOn ? key.applyingControl() : key)
        // Armed is one key; locked stays until it is tapped off.
        if state.control == .armed { state.setControl(.off) }
    }

    // MARK: - The row

    fileprivate struct Entry: Identifiable {
        let id: Int
        let glyph: String
        let key: TerminalKey
        /// Only the arrows repeat: scrolling a long agent output one line per
        /// tap is not usable, and a repeating `esc` is a hazard.
        let repeats: Bool
    }

    /// Ordered so the keys that matter are on screen without scrolling on the
    /// narrowest supported device. Glyphs, never words.
    ///
    /// Measured rather than intended. At 13 pt medium monospaced, with 10 pt
    /// horizontal padding, 6 pt spacing and the 41 pt pinned dismiss button,
    /// the caps run: `esc` 44.1, `ctrl` 52.1, `tab` 44.1, each arrow 28.0,
    /// `⌃C` 36.1. The floor for iOS 17 is a 375 pt phone, which leaves 334 pt
    /// of scrollable room — `esc ctrl tab ← ↓ ↑ →` ends at 294.5, so the seven
    /// keys the plan names are visible there with 39 pt to spare, and `⌃C` is
    /// the first key that costs a swipe. It stops costing one at 390 pt.
    /// The whole row is 1157 pt; nothing else is meant to be reachable without
    /// scrolling.
    fileprivate static let escape = Entry(id: -1, glyph: "esc", key: .escape, repeats: false)

    fileprivate static let keys: [Entry] = {
        var index = 0
        func entry(_ glyph: String, _ key: TerminalKey, repeats: Bool = false) -> Entry {
            defer { index += 1 }
            return Entry(id: index, glyph: glyph, key: key, repeats: repeats)
        }
        return [
            entry("tab", .tab),
            entry("←", .left, repeats: true),
            entry("↓", .down, repeats: true),
            entry("↑", .up, repeats: true),
            entry("→", .right, repeats: true),
            entry("⌃C", .control("c")),
            entry("|", .literal("|")),
            entry("~", .literal("~")),
            entry("/", .literal("/")),
            entry("-", .literal("-")),
            entry("_", .literal("_")),
            entry("`", .literal("`")),
            entry("{", .literal("{")),
            entry("}", .literal("}")),
            entry("[", .literal("[")),
            entry("]", .literal("]")),
            entry("<", .literal("<")),
            entry(">", .literal(">")),
            entry("$", .literal("$")),
            entry("&", .literal("&")),
            entry("*", .literal("*")),
            entry("⌃D", .control("d")),
            entry("⌃Z", .control("z")),
            entry("⌃R", .control("r")),
            entry("⌃L", .control("l")),
            entry("⇞", .pageUp, repeats: true),
            entry("⇟", .pageDown, repeats: true),
            entry("⇱", .home),
            entry("⇲", .end)
        ]
    }()
}

/// One capsule.
///
/// Press-down rather than tap-up, and a light impact with it, because that is
/// what the system keyboard does and a terminal key that fires on release
/// feels broken next to it.
private struct KeyCap: View {
    let glyph: String
    let font: Font
    let height: CGFloat
    let padding: CGFloat
    let isOn: Bool
    let repeats: Bool
    var longPressDuration: Double?
    let action: () -> Void
    var onLongPress: (() -> Void)?

    @State private var pressed = false
    @State private var longPressFired = false
    @State private var repeatTask: Task<Void, Never>?
    @State private var longPressTask: Task<Void, Never>?

    private static let repeatDelay = Duration.milliseconds(400)
    private static let repeatInterval = Duration.milliseconds(60)

    var body: some View {
        Text(glyph)
            .font(font)
            .foregroundStyle(isOn ? Color.white : Color.primary)
            .padding(.horizontal, padding)
            .frame(height: height)
            .background(
                Capsule().fill(
                    isOn
                        ? AnyShapeStyle(Color.accentColor)
                        : AnyShapeStyle(pressed ? .quaternary : .quinary)
                )
            )
            .contentShape(Capsule())
            // A zero-distance drag rather than a tap: a tap gesture cannot tell
            // us when the finger went down, and both the haptic and the repeat
            // timer start there.
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in begin() }
                    .onEnded { _ in end() }
            )
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel(glyph)
    }

    private func begin() {
        guard !pressed else { return }
        pressed = true
        longPressFired = false
        haptic()

        if let longPressDuration, onLongPress != nil {
            // A key with a long-press meaning cannot also fire on press-down;
            // it would emit before the user finished saying which they meant.
            longPressTask = Task { @MainActor in
                try? await Task.sleep(for: .seconds(longPressDuration))
                guard !Task.isCancelled else { return }
                longPressFired = true
                haptic()
                onLongPress?()
            }
        } else {
            action()
            if repeats { startRepeating() }
        }
    }

    private func end() {
        pressed = false
        repeatTask?.cancel()
        repeatTask = nil
        longPressTask?.cancel()
        longPressTask = nil
        if longPressDuration != nil, onLongPress != nil, !longPressFired {
            action()
        }
    }

    private func startRepeating() {
        repeatTask = Task { @MainActor in
            try? await Task.sleep(for: Self.repeatDelay)
            while !Task.isCancelled {
                action()
                try? await Task.sleep(for: Self.repeatInterval)
            }
        }
    }

    private func haptic() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }
}

/// Hosts the bar as a real `inputAccessoryView`, so it rides with the keyboard
/// rather than being laid out above it and left behind on dismissal.
///
/// Phase 5 hands this to SwiftTerm's iOS view, replacing the accessory toolbar
/// that ships with it.
final class TerminalKeyBarAccessory: UIInputView {
    private let host: UIHostingController<TerminalKeyBar>

    /// The sticky modifier, reachable from the surface that has to reset it.
    let state: TerminalKeyBarState

    init(
        state: TerminalKeyBarState = TerminalKeyBarState(),
        onKey: @escaping (TerminalKey) -> Void,
        onDismiss: @escaping () -> Void
    ) {
        self.state = state
        host = UIHostingController(rootView: TerminalKeyBar(onKey: onKey, onDismiss: onDismiss, state: state))
        super.init(
            frame: CGRect(x: 0, y: 0, width: UIScreen.main.bounds.width, height: Self.height),
            inputViewStyle: .keyboard
        )
        // Frame-based, matching SwiftTerm's own `TerminalAccessory`: an input
        // accessory view is positioned and width-matched to the keyboard by
        // UIKit, and a root view that has opted out of autoresizing has no
        // width of its own until the keyboard's own layout pass supplies one.
        // `allowsSelfSizing` is what lets the fixed height survive that pass.
        allowsSelfSizing = true
        autoresizingMask = .flexibleWidth

        host.view.backgroundColor = .clear
        host.view.frame = bounds
        host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(host.view)
    }

    /// The bar is exactly one row tall, whatever the keyboard is doing.
    override var intrinsicContentSize: CGSize {
        CGSize(width: UIView.noIntrinsicMetric, height: Self.height)
    }

    static let height: CGFloat = 34

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }
}

#Preview {
    let surface = StubTerminalSurface()
    let state = TerminalKeyBarState()
    state.onControlChange = { surface.setControlModifier($0) }
    return VStack(spacing: 0) {
        StubTerminalSurfaceView(surface: surface)
        TerminalKeyBar(
            onKey: { surface.onInput(ArraySlice(surface.encode($0))) },
            onDismiss: {},
            state: state
        )
    }
}
