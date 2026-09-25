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

/// The row itself.
///
/// UIKit rather than SwiftUI, and that is the fix rather than a preference. A
/// cap has to know when the finger went down — the haptic, the key and the
/// repeat timer all start there — and the only SwiftUI gesture that reports
/// that is a zero-distance drag, which claims the touch the moment it lands.
/// The row then never scrolled: every swipe began on a cap. A `UIScrollView`
/// full of `UIControl`s gets the arbitration for free. With
/// `delaysContentTouches` it holds a new touch for a moment to see whether it
/// is a swipe; a swipe pans and the cap never hears of it, a press is handed
/// to the cap as `.touchDown`. A quick tap is delivered the moment it lifts,
/// so nothing a user can feel is lost — and a swipe that starts on `esc` does
/// not send one, which firing on raw finger-down would.
final class TerminalKeyBarView: UIView, UIScrollViewDelegate {
    /// A logical key press, already control-modified if the sticky modifier
    /// was armed. The caller encodes it through the surface.
    private let onKey: (TerminalKey) -> Void
    private let onDismiss: () -> Void
    /// The sticky modifier's state, held outside the view.
    ///
    /// It lives in a reference type because the *surface* also resets it: a
    /// letter typed on the system keyboard while `ctrl` is armed spends the
    /// modifier, and a cap still lit after that reads as locked.
    let state: TerminalKeyBarState

    /// The space budget, as numbers rather than as an intention. One row is
    /// the whole allowance: a second would be a third of the visible terminal
    /// on a small phone.
    fileprivate enum Metrics {
        static let barHeight: CGFloat = 34
        static let keyHeight: CGFloat = 28
        static let keyPadding: CGFloat = 10
        static let spacing: CGFloat = 6
        static let dismissWidth: CGFloat = 40
        /// How far the row fades out at an edge that has more keys past it.
        /// Narrow enough that `→` stays whole on a 375 pt phone.
        static let fadeWidth: CGFloat = 18
        static let font = UIFont.monospacedSystemFont(ofSize: 13, weight: .medium)

        static let repeatDelay: TimeInterval = 0.4
        static let repeatInterval: TimeInterval = 0.06
        static let longPressDuration: TimeInterval = 0.5
    }

    private let scrollView = KeyBarScrollView()
    private let fadeContainer = UIView()
    private let fadeMask = CAGradientLayer()
    private let controlCap = KeyCapButton(glyph: "ctrl")
    private let haptics = UIImpactFeedbackGenerator(style: .light)
    private var repeatTimer: Timer?

    init(
        state: TerminalKeyBarState,
        onKey: @escaping (TerminalKey) -> Void,
        onDismiss: @escaping () -> Void
    ) {
        self.state = state
        self.onKey = onKey
        self.onDismiss = onDismiss
        super.init(frame: .zero)
        build()
        observeControl()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }

    deinit { repeatTimer?.invalidate() }

    // MARK: - Layout

    private func build() {
        // The keyboard's own colour rather than a material. A blur over the
        // black terminal read as a grey band stacked on a lighter keyboard; a
        // solid match makes bar and keyboard one surface.
        let background = UIView()
        background.backgroundColor = KeyboardPalette.background

        scrollView.showsHorizontalScrollIndicator = false
        scrollView.showsVerticalScrollIndicator = false
        scrollView.alwaysBounceHorizontal = true
        scrollView.alwaysBounceVertical = false
        scrollView.delaysContentTouches = true
        scrollView.canCancelContentTouches = true
        scrollView.scrollsToTop = false
        scrollView.delegate = self

        // `esc  ctrl  tab  ← ↓ ↑ →  ⌃C  …`. `ctrl` is built separately from
        // the rest because it is a modifier with its own state, not an entry in
        // the key table.
        let row = UIStackView(arrangedSubviews: [cap(Self.escape), controlCap] + Self.keys.map(cap))
        row.axis = .horizontal
        row.alignment = .center
        row.spacing = Metrics.spacing
        // The order is a layout, not reading-order text: `←` stays on the left.
        row.semanticContentAttribute = .forceLeftToRight
        configureControlCap()

        // The whole row scrolls, with no leading pinned group: pinning both
        // ends would cost ~90 pt of scrollable width to save one swipe.
        fadeContainer.layer.mask = fadeMask
        fadeMask.startPoint = CGPoint(x: 0, y: 0.5)
        fadeMask.endPoint = CGPoint(x: 1, y: 0.5)

        // Pinned to the trailing edge so it never scrolls away: a user reading
        // output gets the whole screen back, and a tap on the terminal brings
        // the keyboard and this row back together.
        let divider = UIView()
        divider.backgroundColor = .separator
        let dismiss = UIButton(type: .system)
        dismiss.setImage(
            UIImage(
                systemName: "keyboard.chevron.compact.down",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 15, weight: .medium)
            ),
            for: .normal
        )
        dismiss.tintColor = .label
        dismiss.accessibilityLabel = "Hide keyboard"
        dismiss.addAction(UIAction { [weak self] _ in self?.onDismiss() }, for: .touchUpInside)

        for view in [background, fadeContainer, scrollView, row, divider, dismiss] as [UIView] {
            view.translatesAutoresizingMaskIntoConstraints = false
        }
        addSubview(background)
        addSubview(fadeContainer)
        fadeContainer.addSubview(scrollView)
        scrollView.addSubview(row)
        addSubview(divider)
        addSubview(dismiss)

        let content = scrollView.contentLayoutGuide
        let frame = scrollView.frameLayoutGuide
        NSLayoutConstraint.activate([
            background.leadingAnchor.constraint(equalTo: leadingAnchor),
            background.trailingAnchor.constraint(equalTo: trailingAnchor),
            background.topAnchor.constraint(equalTo: topAnchor),
            background.bottomAnchor.constraint(equalTo: bottomAnchor),

            fadeContainer.leadingAnchor.constraint(equalTo: leadingAnchor),
            fadeContainer.topAnchor.constraint(equalTo: topAnchor),
            fadeContainer.bottomAnchor.constraint(equalTo: bottomAnchor),
            fadeContainer.trailingAnchor.constraint(equalTo: divider.leadingAnchor),

            scrollView.leadingAnchor.constraint(equalTo: fadeContainer.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: fadeContainer.trailingAnchor),
            scrollView.topAnchor.constraint(equalTo: fadeContainer.topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: fadeContainer.bottomAnchor),

            row.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: Metrics.spacing),
            row.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Metrics.spacing),
            row.topAnchor.constraint(equalTo: content.topAnchor),
            row.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            row.heightAnchor.constraint(equalTo: frame.heightAnchor),

            divider.topAnchor.constraint(equalTo: topAnchor),
            divider.bottomAnchor.constraint(equalTo: bottomAnchor),
            divider.widthAnchor.constraint(equalToConstant: 1 / max(traitCollection.displayScale, 1)),
            divider.trailingAnchor.constraint(equalTo: dismiss.leadingAnchor),

            dismiss.topAnchor.constraint(equalTo: topAnchor),
            dismiss.bottomAnchor.constraint(equalTo: bottomAnchor),
            dismiss.trailingAnchor.constraint(equalTo: trailingAnchor),
            dismiss.widthAnchor.constraint(equalToConstant: Metrics.dismissWidth),

            heightAnchor.constraint(equalToConstant: Metrics.barHeight).withPriority(.defaultHigh)
        ])
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        updateFades()
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        updateFades()
    }

    /// Fades an edge only while there are keys past it, so a faded `⌃C` at
    /// the trailing edge is what says the row goes on.
    private func updateFades() {
        let width = fadeContainer.bounds.width
        guard width > 0 else { return }
        let offset = scrollView.contentOffset.x
        let maxOffset = scrollView.contentSize.width - scrollView.bounds.width
        let opaque = UIColor.black.cgColor
        let clear = UIColor.clear.cgColor
        let edge = NSNumber(value: Double(min(Metrics.fadeWidth / width, 0.5)))

        CATransaction.begin()
        CATransaction.setDisableActions(true)
        fadeMask.frame = fadeContainer.bounds
        fadeMask.colors = [offset > 0.5 ? clear : opaque, opaque, opaque, offset < maxOffset - 0.5 ? clear : opaque]
        fadeMask.locations = [0, edge, NSNumber(value: 1 - edge.doubleValue), 1]
        CATransaction.commit()
    }

    // MARK: - Caps

    /// Press-down rather than tap-up, and a light impact with it, because that
    /// is what the system keyboard does and a terminal key that fires on
    /// release feels broken next to it.
    private func cap(_ entry: Entry) -> KeyCapButton {
        let button = KeyCapButton(glyph: entry.glyph)
        button.addAction(UIAction { [weak self] _ in
            guard let self else { return }
            haptic()
            press(entry.key)
            if entry.repeats { startRepeating(entry.key) }
        }, for: .touchDown)
        button.addAction(UIAction { [weak self] _ in self?.stopRepeating() }, for: [.touchUpInside, .touchUpOutside, .touchCancel])
        return button
    }

    private func press(_ key: TerminalKey) {
        // The bar encodes its own keys; the surface's modifier is for the
        // system keyboard's letters, and both must not fire on one press.
        onKey(state.control.isOn ? key.applyingControl() : key)
        // Armed is one key; locked stays until it is tapped off.
        if state.control == .armed { state.setControl(.off) }
    }

    /// Only the arrows and pages repeat; see `Entry.repeats`. Timers run in the
    /// common modes so a held arrow keeps going while UIKit is tracking.
    private func startRepeating(_ key: TerminalKey) {
        stopRepeating()
        let delay = Timer(timeInterval: Metrics.repeatDelay, repeats: false) { [weak self] _ in
            guard let self else { return }
            let interval = Timer(timeInterval: Metrics.repeatInterval, repeats: true) { [weak self] _ in
                self?.press(key)
            }
            repeatTimer = interval
            RunLoop.main.add(interval, forMode: .common)
            press(key)
        }
        repeatTimer = delay
        RunLoop.main.add(delay, forMode: .common)
    }

    private func stopRepeating() {
        repeatTimer?.invalidate()
        repeatTimer = nil
    }

    private func haptic() {
        haptics.impactOccurred()
        haptics.prepare()
    }

    // MARK: - The sticky modifier

    /// `ctrl` is a modifier rather than a key so the bar needs one `⌃` and not
    /// a control variant of every letter. Tap arms it for the next press;
    /// long-press locks it until tapped again.
    ///
    /// It cannot fire on press-down like the other caps: it would act before
    /// the user finished saying which they meant. The long-press recognizer
    /// cancels the button's touch when it fires, so a lock never also toggles.
    private func configureControlCap() {
        controlCap.accessibilityLabel = "Control"
        controlCap.addAction(UIAction { [weak self] _ in self?.haptic() }, for: .touchDown)
        controlCap.addAction(UIAction { [weak self] _ in
            guard let self else { return }
            state.setControl(state.control.isOn ? .off : .armed)
        }, for: .touchUpInside)

        let longPress = UILongPressGestureRecognizer(target: self, action: #selector(controlLongPressed(_:)))
        longPress.minimumPressDuration = Metrics.longPressDuration
        longPress.cancelsTouchesInView = true
        controlCap.addGestureRecognizer(longPress)
    }

    @objc private func controlLongPressed(_ recognizer: UILongPressGestureRecognizer) {
        guard recognizer.state == .began else { return }
        haptic()
        state.setControl(state.control == .locked ? .off : .locked)
    }

    /// Repaints the cap whenever the modifier changes, from either side — the
    /// cap, or the surface spending it on a system-keyboard letter.
    private func observeControl() {
        withObservationTracking {
            let control = state.control
            controlCap.isOn = control.isOn
            controlCap.accessibilityValue = switch control {
            case .off: "off"
            case .armed: "armed for the next key"
            case .locked: "locked"
            }
        } onChange: { [weak self] in
            DispatchQueue.main.async { self?.observeControl() }
        }
    }

    // MARK: - The row

    fileprivate struct Entry {
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
    /// scrolling. The 18 pt trailing fade sits inside those 39 pt.
    fileprivate static let escape = Entry(glyph: "esc", key: .escape, repeats: false)

    fileprivate static let keys: [Entry] = {
        func entry(_ glyph: String, _ key: TerminalKey, repeats: Bool = false) -> Entry {
            Entry(glyph: glyph, key: key, repeats: repeats)
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

/// Lets a swipe that outlasts the touch delay still become a scroll.
///
/// `UIScrollView` refuses to cancel a touch a `UIControl` already holds, so a
/// finger that rested on a cap and then moved would be stuck on that cap. The
/// cap has already sent its key by then; cancelling only stops its repeat.
private final class KeyBarScrollView: UIScrollView {
    override func touchesShouldCancel(in view: UIView) -> Bool { true }
}

/// One capsule.
private final class KeyCapButton: UIButton {
    typealias Metrics = TerminalKeyBarView.Metrics

    /// Lit: the sticky modifier is armed or locked.
    var isOn = false {
        didSet { if isOn != oldValue { setNeedsUpdateConfiguration() } }
    }

    init(glyph: String) {
        super.init(frame: .zero)
        var config = UIButton.Configuration.plain()
        config.attributedTitle = AttributedString(glyph, attributes: AttributeContainer([.font: Metrics.font]))
        config.contentInsets = NSDirectionalEdgeInsets(
            top: 0, leading: Metrics.keyPadding, bottom: 0, trailing: Metrics.keyPadding
        )
        config.titlePadding = 0
        config.cornerStyle = .capsule
        configuration = config
        configurationUpdateHandler = { button in
            guard let cap = button as? KeyCapButton, var config = cap.configuration else { return }
            config.baseForegroundColor = cap.isOn ? .white : .label
            config.background.backgroundColor = cap.isOn
                ? cap.tintColor
                : (cap.isHighlighted ? KeyboardPalette.pressedKey : KeyboardPalette.key)
            cap.configuration = config
        }
        heightAnchor.constraint(equalToConstant: Metrics.keyHeight).isActive = true
        setContentCompressionResistancePriority(.required, for: .horizontal)
        setContentHuggingPriority(.required, for: .horizontal)
        accessibilityLabel = glyph
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }
}

/// The system keyboard's colours, so the bar and the backdrop behind the
/// keyboard read as part of it rather than as app chrome beside it.
///
/// Sampled, not named: UIKit publishes no keyboard colour. The keyboard is
/// translucent, so these are what it shows over a backdrop of its own colour
/// — which is what `TerminalView` puts behind it.
enum KeyboardPalette {
    static let background = UIColor { traits in
        traits.userInterfaceStyle == .dark
            ? UIColor(red: 0.125, green: 0.125, blue: 0.129, alpha: 1)
            : UIColor(red: 0.847, green: 0.855, blue: 0.871, alpha: 1)
    }

    static let key = UIColor { traits in
        traits.userInterfaceStyle == .dark
            ? UIColor(white: 1, alpha: 0.16)
            : UIColor(red: 0.988, green: 0.988, blue: 0.996, alpha: 1)
    }

    static let pressedKey = UIColor { traits in
        traits.userInterfaceStyle == .dark
            ? UIColor(white: 1, alpha: 0.35)
            : UIColor(red: 0.690, green: 0.702, blue: 0.722, alpha: 1)
    }
}

extension View {
    /// Black down to the top of the keyboard, the keyboard's colour beneath.
    ///
    /// The keyboard's top corners are rounded, so whatever sits behind it
    /// shows through them. A black that ignored the keyboard's safe area left
    /// two dark notches under the bar; this layer ignores only the container's,
    /// so it stops where the keyboard starts and the backdrop fills the rest.
    func terminalKeyboardBackdrop() -> some View {
        background {
            Color.black
                .ignoresSafeArea(.container)
                .background(Color(uiColor: KeyboardPalette.background).ignoresSafeArea())
        }
    }
}

private extension NSLayoutConstraint {
    func withPriority(_ priority: UILayoutPriority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}

/// Hosts the bar as a real `inputAccessoryView`, so it rides with the keyboard
/// rather than being laid out above it and left behind on dismissal.
///
/// Phase 5 hands this to SwiftTerm's iOS view, replacing the accessory toolbar
/// that ships with it.
final class TerminalKeyBarAccessory: UIInputView {
    private let bar: TerminalKeyBarView

    /// The sticky modifier, reachable from the surface that has to reset it.
    var state: TerminalKeyBarState { bar.state }

    init(
        state: TerminalKeyBarState = TerminalKeyBarState(),
        onKey: @escaping (TerminalKey) -> Void,
        onDismiss: @escaping () -> Void
    ) {
        bar = TerminalKeyBarView(state: state, onKey: onKey, onDismiss: onDismiss)
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

        bar.translatesAutoresizingMaskIntoConstraints = true
        bar.frame = bounds
        bar.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(bar)
    }

    /// The bar is exactly one row tall, whatever the keyboard is doing.
    override var intrinsicContentSize: CGSize {
        CGSize(width: UIView.noIntrinsicMetric, height: Self.height)
    }

    static let height: CGFloat = TerminalKeyBarView.Metrics.barHeight

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }
}

/// The bar in a SwiftUI canvas.
private struct TerminalKeyBarPreview: UIViewRepresentable {
    let state: TerminalKeyBarState
    let onKey: (TerminalKey) -> Void

    func makeUIView(context: Context) -> TerminalKeyBarView {
        TerminalKeyBarView(state: state, onKey: onKey, onDismiss: {})
    }

    func updateUIView(_ uiView: TerminalKeyBarView, context: Context) {}
}

#Preview {
    let surface = StubTerminalSurface()
    let state = TerminalKeyBarState()
    state.onControlChange = { surface.setControlModifier($0) }
    return VStack(spacing: 0) {
        StubTerminalSurfaceView(surface: surface)
        TerminalKeyBarPreview(state: state, onKey: { surface.onInput(ArraySlice(surface.encode($0))) })
            .frame(height: TerminalKeyBarAccessory.height)
    }
}
