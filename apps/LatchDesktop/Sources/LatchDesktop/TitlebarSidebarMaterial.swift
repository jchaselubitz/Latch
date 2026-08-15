import SwiftUI
import AppKit

/// Extends the sidebar's material across the whole titlebar while the sidebar is showing.
///
/// AppKit paints sidebar material in the titlebar only above the sidebar column; the rest of
/// the toolbar gets the window's own background, so the header reads as two different
/// surfaces. There is no public switch for "use one material across the titlebar", so this
/// view makes the titlebar transparent and slides a full-width `NSVisualEffectView` using the
/// same `.sidebar` material underneath it. Toolbar items live in the titlebar view above, so
/// they keep drawing normally.
///
/// When the split view's sidebar collapses there is nothing left to match, so the backdrop is
/// removed and the window goes back to its standard titlebar — the app's primary window color.
///
/// The view itself draws nothing and takes no clicks; it exists only as a handle on the
/// window, so it belongs in a `.background`.
struct TitlebarSidebarMaterial: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let probe = WindowProbeView()
        probe.onWindowChange = { [weak coordinator = context.coordinator] window in
            coordinator?.attach(to: window)
        }
        return probe
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.attach(to: nsView.window)
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.detach()
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator: NSObject {
        private weak var hostWindow: NSWindow?
        private weak var splitView: NSSplitView?
        private var backdrop: NSVisualEffectView?
        private var observers: [NSObjectProtocol] = []

        /// The window's own titlebar settings, captured before anything is overridden so the
        /// collapsed state can restore exactly what the window started with.
        private var defaultTitlebarAppearsTransparent: Bool?
        private var defaultTitlebarSeparatorStyle: NSTitlebarSeparatorStyle?

        /// The split view is only reachable once SwiftUI has built the column hierarchy, which
        /// can be a few runloop turns after the window appears.
        private static let maximumAttempts = 20
        private static let retryDelay: TimeInterval = 0.1
        private var pendingAttempts = 0

        func attach(to window: NSWindow?) {
            guard let window else { return }
            guard let splitView = WindowSplitViewLocator.firstSplitView(in: window.contentView) else {
                scheduleRetry(for: window)
                return
            }

            if hostWindow !== window || self.splitView !== splitView {
                removeObservers()
                hostWindow = window
                self.splitView = splitView
                captureDefaults(of: window)
                addObservers(window: window, splitView: splitView)
            }

            synchronize()
        }

        func detach() {
            removeObservers()
            removeBackdrop()
            restoreTitlebarDefaults()
            hostWindow = nil
            splitView = nil
            defaultTitlebarAppearsTransparent = nil
            defaultTitlebarSeparatorStyle = nil
        }

        // MARK: - State

        private var isSidebarVisible: Bool {
            guard let splitView, let sidebar = splitView.arrangedSubviews.first else { return false }
            // A dragged-shut column reports a zero width without ever being marked collapsed,
            // so width is the reliable signal and `isSubviewCollapsed` is the fast path.
            if splitView.isSubviewCollapsed(sidebar) { return false }
            return sidebar.frame.width > 1
        }

        /// Installs or removes the backdrop so it matches whether the sidebar is on screen, and
        /// keeps it lined up with the current titlebar height.
        private func synchronize() {
            guard let window = hostWindow else { return }
            if isSidebarVisible {
                installBackdrop(in: window)
                layoutBackdrop(in: window)
            } else {
                removeBackdrop()
                restoreTitlebarDefaults()
            }
        }

        // MARK: - Backdrop

        private func installBackdrop(in window: NSWindow) {
            // The titlebar is a sibling of the content view inside the window's frame view, so
            // that is the only place a view can sit behind the toolbar and still be clipped to
            // the window's rounded corners.
            guard let frameView = window.contentView?.superview else { return }

            window.titlebarAppearsTransparent = true
            // The sidebar has no hairline under the toolbar; leaving the separator on would draw
            // one straight across the material we just unified.
            window.titlebarSeparatorStyle = .none

            if let backdrop, backdrop.superview === frameView { return }

            let effect = TitlebarBackdropView()
            effect.material = .sidebar
            effect.blendingMode = .behindWindow
            effect.state = .followsWindowActiveState
            effect.autoresizingMask = [.width, .minYMargin]
            frameView.addSubview(effect, positioned: .below, relativeTo: nil)
            backdrop = effect
        }

        private func removeBackdrop() {
            backdrop?.removeFromSuperview()
            backdrop = nil
        }

        private func layoutBackdrop(in window: NSWindow) {
            guard let backdrop, let frameView = backdrop.superview else { return }
            let titlebarHeight = max(0, frameView.bounds.height - window.contentLayoutRect.height)
            backdrop.frame = NSRect(
                x: 0,
                y: frameView.bounds.height - titlebarHeight,
                width: frameView.bounds.width,
                height: titlebarHeight
            )
        }

        private func captureDefaults(of window: NSWindow) {
            if defaultTitlebarAppearsTransparent == nil {
                defaultTitlebarAppearsTransparent = window.titlebarAppearsTransparent
            }
            if defaultTitlebarSeparatorStyle == nil {
                defaultTitlebarSeparatorStyle = window.titlebarSeparatorStyle
            }
        }

        private func restoreTitlebarDefaults() {
            guard let window = hostWindow else { return }
            if let transparent = defaultTitlebarAppearsTransparent {
                window.titlebarAppearsTransparent = transparent
            }
            if let separatorStyle = defaultTitlebarSeparatorStyle {
                window.titlebarSeparatorStyle = separatorStyle
            }
        }

        // MARK: - Observation

        private func addObservers(window: NSWindow, splitView: NSSplitView) {
            let center = NotificationCenter.default
            // Collapsing, expanding, and dragging the sidebar all land here.
            observers.append(center.addObserver(
                forName: NSSplitView.didResizeSubviewsNotification,
                object: splitView,
                queue: .main
            ) { [weak self] _ in
                self?.synchronize()
            })
            // The titlebar changes height with the window's toolbar style and full-screen state.
            for name in [NSWindow.didResizeNotification, NSWindow.didEnterFullScreenNotification, NSWindow.didExitFullScreenNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    self?.synchronize()
                })
            }
        }

        private func removeObservers() {
            let center = NotificationCenter.default
            observers.forEach { center.removeObserver($0) }
            observers.removeAll()
        }

        private func scheduleRetry(for window: NSWindow) {
            guard pendingAttempts < Self.maximumAttempts else { return }
            pendingAttempts += 1
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.retryDelay) { [weak self, weak window] in
                self?.attach(to: window)
            }
        }
    }

    /// Pure backdrop: it sits under the titlebar and must not swallow toolbar clicks or the
    /// window drag that the titlebar itself handles.
    private final class TitlebarBackdropView: NSVisualEffectView {
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}
