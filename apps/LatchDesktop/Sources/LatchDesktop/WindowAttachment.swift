import SwiftUI
import AppKit

/// Reports the window as soon as the view is planted in one; an `NSViewRepresentable`
/// has no window at `makeNSView` time.
///
/// The view draws nothing and takes no clicks: it exists only as a handle on the window
/// for representables that need to reach AppKit, so it belongs in a `.background`.
final class WindowProbeView: NSView {
    var onWindowChange: ((NSWindow?) -> Void)?

    override var isOpaque: Bool { false }
    override var intrinsicContentSize: NSSize { .zero }

    /// Purely a window handle: it must never take a click away from the view underneath.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        onWindowChange?(window)
    }
}

enum WindowSplitViewLocator {
    /// The `NSSplitView` SwiftUI builds for `NavigationSplitView`, found by walking the
    /// window's view tree. Only a split view with real arranged columns counts, so the
    /// caller can keep looking while SwiftUI is still assembling the hierarchy.
    static func firstSplitView(in view: NSView?) -> NSSplitView? {
        guard let view else { return nil }
        if let split = view as? NSSplitView, split.arrangedSubviews.count >= 2 {
            return split
        }
        for subview in view.subviews {
            if let split = firstSplitView(in: subview) { return split }
        }
        return nil
    }
}
