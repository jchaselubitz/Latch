import SwiftUI
import AppKit

/// Pins the toolbar's sidebar/detail boundary to the split view divider underneath it.
///
/// `NavigationSplitView` gives the sidebar a full-height column, but SwiftUI never
/// installs AppKit's tracking separator in the window toolbar. Without it the toolbar
/// paints its detail background from a point a couple of points to the left of the
/// divider, so in the titlebar the sidebar looks narrower than it is and its trailing
/// border stops where the toolbar begins. `NSTrackingSeparatorToolbarItem` is the
/// AppKit item that exists for exactly this: it locks the boundary to the divider and
/// carries the border up through the titlebar, so the sidebar reads as one column from
/// the traffic lights to the bottom of the window.
///
/// The view itself draws nothing and takes no clicks; it exists only as a handle on the
/// window, so it belongs in a `.background`.
struct SidebarToolbarSeparator: NSViewRepresentable {
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

    /// Reports the window as soon as the view is planted in one; a representable has no
    /// window at `makeNSView` time.
    private final class WindowProbeView: NSView {
        var onWindowChange: ((NSWindow?) -> Void)?

        override var isOpaque: Bool { false }
        override var intrinsicContentSize: NSSize { .zero }

        /// Purely a window handle: it must never take a click away from the split view.
        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            onWindowChange?(window)
        }
    }

    /// Owns the toolbar delegate proxying. SwiftUI keeps its own toolbar delegate, so the
    /// coordinator wraps it rather than replacing it: everything except the separator
    /// identifier is forwarded untouched.
    final class Coordinator: NSObject, NSToolbarDelegate {
        private weak var hostWindow: NSWindow?
        private weak var toolbar: NSToolbar?
        private weak var splitView: NSSplitView?
        private weak var wrappedDelegate: NSToolbarDelegate?
        private var pendingAttempts = 0
        /// Set once a tracking separator this coordinator did not insert is seen in the toolbar.
        /// SwiftUI takes its own separator down and puts it back when it rebuilds the toolbar, and
        /// slipping ours into that gap would abort the app the moment SwiftUI re-registers its own.
        private var separatorIsProvidedBySystem = false

        /// Toolbar item views are laid out asynchronously, and the insertion point is read
        /// from their frames, so a first look right after the window appears can come up
        /// empty. Retry for a couple of seconds before giving up.
        private static let maximumAttempts = 20
        private static let retryDelay: TimeInterval = 0.1

        func attach(to window: NSWindow?) {
            guard let window, let toolbar = window.toolbar else { return }
            guard let splitView = Self.firstSplitView(in: window.contentView) else {
                scheduleRetry(for: window)
                return
            }

            self.hostWindow = window
            self.toolbar = toolbar
            self.splitView = splitView

            if toolbar.delegate !== self {
                wrappedDelegate = toolbar.delegate
                toolbar.delegate = self
            }

            if !installSeparator() {
                scheduleRetry(for: window)
            }
        }

        func detach() {
            if let toolbar, toolbar.delegate === self {
                toolbar.delegate = wrappedDelegate
            }
            hostWindow = nil
            toolbar = nil
            splitView = nil
            wrappedDelegate = nil
            separatorIsProvidedBySystem = false
        }

        // MARK: - Installation

        /// Returns `false` when the toolbar is not laid out far enough yet to say where the
        /// separator belongs, so the caller can try again.
        @discardableResult
        private func installSeparator() -> Bool {
            guard let toolbar, let splitView else { return false }
            // AppKit raises `NSInternalInconsistencyException` — "Cannot register more than one
            // NSTrackingSeparatorToolbarItem that tracks the same divider" — from
            // `insertItem(withItemIdentifier:at:)`, and an exception out of AppKit terminates the
            // app. Since macOS 26, SwiftUI installs the tracking separator itself under its own
            // identifier (`com.apple.SwiftUI.splitViewSeparator-0`), so matching on our identifier
            // alone misses it and the second insert aborts the process at launch. Match on the
            // item class instead: any tracking separator already in the toolbar means the boundary
            // is handled and there is nothing left for this view to do.
            if separatorIsProvidedBySystem { return true }
            if toolbar.items.contains(where: { $0 is NSTrackingSeparatorToolbarItem }) {
                separatorIsProvidedBySystem = !toolbar.items.contains {
                    $0 is NSTrackingSeparatorToolbarItem && $0.itemIdentifier == .sidebarTrackingSeparator
                }
                return true
            }
            guard let index = detailSectionIndex(in: toolbar, splitView: splitView) else {
                return false
            }
            toolbar.insertItem(withItemIdentifier: .sidebarTrackingSeparator, at: index)
            return toolbar.items.contains { $0 is NSTrackingSeparatorToolbarItem }
        }

        private func scheduleRetry(for window: NSWindow) {
            guard pendingAttempts < Self.maximumAttempts else { return }
            pendingAttempts += 1
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.retryDelay) { [weak self, weak window] in
                self?.attach(to: window)
            }
        }

        /// The index of the first toolbar item that already sits on the detail side of the
        /// divider — where the separator has to go so the items on either side of it stay
        /// where they are. `nil` when nothing in the toolbar can be placed yet, which means it
        /// has not been laid out and the caller should look again.
        private func detailSectionIndex(in toolbar: NSToolbar, splitView: NSSplitView) -> Int? {
            guard let sidebar = splitView.arrangedSubviews.first else { return nil }
            let dividerX = splitView.convert(NSPoint(x: sidebar.frame.maxX, y: 0), to: nil).x
            // The misalignment being fixed here means the detail section can start a couple of
            // points left of the divider; no sidebar item ever comes that close to it.
            let boundary = dividerX - 6

            var sawLaidOutItem = false
            for (index, item) in toolbar.items.enumerated() {
                guard let view = item.view, view.window != nil, view.frame.width > 0 else { continue }
                sawLaidOutItem = true
                if view.convert(view.bounds, to: nil).minX >= boundary {
                    return index
                }
            }
            if sawLaidOutItem {
                // Every item is on the sidebar side: the separator belongs after all of them.
                return toolbar.items.count
            }
            // No item exposes a frame to measure. The sidebar toggle is the one item that is
            // always the first thing in the detail section, so it can stand in for geometry.
            return toolbar.items.firstIndex {
                $0.itemIdentifier.rawValue.localizedCaseInsensitiveContains("toggleSidebar")
            }
        }

        /// SwiftUI rebuilds its items whenever the toolbar's content changes and can take the
        /// separator down with them, so put it back once that pass settles.
        private func reinstallSeparatorAfterRebuild() {
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.pendingAttempts = 0
                if !self.installSeparator(), let window = self.hostWindow {
                    self.scheduleRetry(for: window)
                }
            }
        }

        private static func firstSplitView(in view: NSView?) -> NSSplitView? {
            guard let view else { return nil }
            if let split = view as? NSSplitView, split.arrangedSubviews.count >= 2 {
                return split
            }
            for subview in view.subviews {
                if let split = firstSplitView(in: subview) { return split }
            }
            return nil
        }

        // MARK: - NSToolbarDelegate

        func toolbar(
            _ toolbar: NSToolbar,
            itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
            willBeInsertedIntoToolbar flag: Bool
        ) -> NSToolbarItem? {
            if itemIdentifier == .sidebarTrackingSeparator, let splitView {
                return NSTrackingSeparatorToolbarItem(
                    identifier: itemIdentifier,
                    splitView: splitView,
                    dividerIndex: 0
                )
            }
            return wrappedDelegate?.toolbar?(
                toolbar,
                itemForItemIdentifier: itemIdentifier,
                willBeInsertedIntoToolbar: flag
            )
        }

        func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            // Only extend a set SwiftUI actually supplies: inventing one would let a toolbar
            // reset wipe out every real item and leave the separator alone in the titlebar.
            guard let identifiers = wrappedDelegate?.toolbarDefaultItemIdentifiers?(toolbar) else {
                return []
            }
            return allowing(.sidebarTrackingSeparator, in: identifiers)
        }

        func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            allowing(.sidebarTrackingSeparator, in: wrappedDelegate?.toolbarAllowedItemIdentifiers?(toolbar) ?? [])
        }

        func toolbarSelectableItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            wrappedDelegate?.toolbarSelectableItemIdentifiers?(toolbar) ?? []
        }

        func toolbarImmovableItemIdentifiers(_ toolbar: NSToolbar) -> Set<NSToolbarItem.Identifier> {
            wrappedDelegate?.toolbarImmovableItemIdentifiers?(toolbar) ?? []
        }

        func toolbarWillAddItem(_ notification: Notification) {
            wrappedDelegate?.toolbarWillAddItem?(notification)
        }

        func toolbarDidRemoveItem(_ notification: Notification) {
            wrappedDelegate?.toolbarDidRemoveItem?(notification)
            reinstallSeparatorAfterRebuild()
        }

        func toolbar(
            _ toolbar: NSToolbar,
            itemIdentifier: NSToolbarItem.Identifier,
            canBeInsertedAt index: Int
        ) -> Bool {
            if itemIdentifier == .sidebarTrackingSeparator { return true }
            return wrappedDelegate?.toolbar?(toolbar, itemIdentifier: itemIdentifier, canBeInsertedAt: index) ?? true
        }

        private func allowing(
            _ identifier: NSToolbarItem.Identifier,
            in identifiers: [NSToolbarItem.Identifier]
        ) -> [NSToolbarItem.Identifier] {
            identifiers.contains(identifier) ? identifiers : identifiers + [identifier]
        }
    }
}
