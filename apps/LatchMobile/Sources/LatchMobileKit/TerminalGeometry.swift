import CoreGraphics
import Foundation

/// A terminal grid, in columns and rows.
public struct TerminalGrid: Equatable, Sendable {
    public let cols: Int
    public let rows: Int

    public init(cols: Int, rows: Int) {
        self.cols = cols
        self.rows = rows
    }
}

/// The geometry rule: the phone chooses the grid, and prefers the desk's.
///
/// `cols` and `rows` are query parameters on the terminal socket, so nothing
/// requires them to describe the phone's screen. Deriving them from pixels
/// would mean a ~50-column grid in portrait, which resizes the pane,
/// `SIGWINCH`es the child, and asks a layout drawn for 100 columns to survive
/// being halved. Instead the phone picks a grid and renders it at whatever
/// font size fits, panning when it does not.
public enum TerminalGeometry {
    /// Below this the font stops shrinking and the surface pans instead.
    public static let readableFontSize: CGFloat = 11
    /// The smallest grid the readable fit will settle for. A phone narrow
    /// enough to need less than this pans rather than rendering a grid no
    /// full-screen TUI is laid out for.
    public static let minimumGrid = TerminalGrid(cols: 60, rows: 20)

    /// Width of one monospaced cell as a fraction of point size, for the
    /// system monospaced face. Used only to turn a grid into a font size, so a
    /// small inaccuracy costs a fraction of a point rather than a wrong grid.
    public static let advanceRatio: CGFloat = 0.6
    /// Line height as a multiple of point size, same caveat.
    public static let lineHeightRatio: CGFloat = 1.2

    /// The grid to attach at.
    ///
    /// `matchMac` is the default and takes the preview's reported geometry:
    /// attaching at the size the pane already has means the pane does not
    /// resize at all — no `SIGWINCH`, no reflow, and a paused prompt that
    /// cannot repaint transfers exactly as it stands. When there is no preview
    /// there is nothing to match, so it falls through to the readable fit
    /// rather than guessing.
    public static func grid(
        for size: TerminalSize,
        preview: SessionPreview?,
        viewport: CGSize
    ) -> TerminalGrid {
        if let fixed = size.fixedGrid {
            return TerminalGrid(cols: fixed.cols, rows: fixed.rows)
        }
        if size == .matchMac, let preview, preview.cols > 0, preview.rows > 0 {
            return TerminalGrid(cols: preview.cols, rows: preview.rows)
        }
        return readableGrid(viewport: viewport)
    }

    /// The largest grid whose font size stays at or above the readable floor,
    /// never smaller than `minimumGrid`.
    public static func readableGrid(viewport: CGSize) -> TerminalGrid {
        let cellWidth = readableFontSize * advanceRatio
        let cellHeight = readableFontSize * lineHeightRatio
        let cols = viewport.width > 0 ? Int(viewport.width / cellWidth) : 0
        let rows = viewport.height > 0 ? Int(viewport.height / cellHeight) : 0
        return TerminalGrid(
            cols: max(cols, minimumGrid.cols),
            rows: max(rows, minimumGrid.rows)
        )
    }

    /// The pixel size a chosen grid needs at a given font size.
    ///
    /// This is what makes panning real rather than a claim. A renderer lays out
    /// whatever grid fits its own bounds, so a surface merely placed in the
    /// viewport would silently render ~59 columns while the pty was told 100 —
    /// which is exactly the mismatch the geometry rule exists to prevent. The
    /// surface is framed at this size instead and the viewport scrolls over it.
    ///
    /// The width is biased slightly upward because the cell width here is a
    /// ratio rather than the renderer's own metric: rendering a column or two
    /// more than declared leaves them blank, while rendering fewer would clip
    /// the pane. Phase 5 measures the real advance on a device.
    public static func pixelSize(cols: Int, rows: Int, fontSize: CGFloat) -> CGSize {
        CGSize(
            width: ceil(CGFloat(cols) * fontSize * advanceRatio * 1.05),
            height: ceil(CGFloat(rows) * fontSize * lineHeightRatio)
        )
    }

    /// Point size for a chosen grid in a given viewport.
    ///
    /// Font size follows from the grid, not the other way round. Below the
    /// readable floor it stops shrinking and the caller pans horizontally
    /// instead, which is why this clamps rather than returning a hairline.
    public static func fontSize(cols: Int, viewportWidth: CGFloat) -> CGFloat {
        guard cols > 0, viewportWidth > 0 else { return readableFontSize }
        let fitted = viewportWidth / (CGFloat(cols) * advanceRatio)
        return max(fitted, readableFontSize)
    }
}

/// Decides when a resize frame is sent, and it is almost never.
///
/// Each resize `SIGWINCH`es the agent on the Mac and reflows its full-screen
/// TUI, so the viewport is deliberately not an input to the pty's geometry: a
/// phone that opened a terminal must not be visibly worse for the session than
/// one that did not. The keyboard appearing, the device rotating, and the
/// surface reporting the grid it settled on all reach the pty as nothing at
/// all. Only a deliberate grid change — the Settings control, or a "fit to
/// phone" action — emits, and that one is debounced.
@MainActor
public final class TerminalGeometryCoordinator {
    /// The grid currently declared to the gateway.
    public private(set) var grid: TerminalGrid?
    /// Every resize actually emitted, in order. Kept so a caller (and a test)
    /// can assert the count rather than infer it.
    public private(set) var emittedCount = 0
    /// The last viewport seen. It shapes font size and pan extent and nothing
    /// else.
    public private(set) var viewport: CGSize = .zero

    private let emit: @MainActor (TerminalGrid) -> Void
    private let debounce: Duration
    private var pending: Task<Void, Never>?

    /// `debounce` is a parameter so tests do not spend 150 ms per case; the
    /// app uses the default.
    public init(
        debounce: Duration = .milliseconds(150),
        emit: @escaping @MainActor (TerminalGrid) -> Void
    ) {
        self.debounce = debounce
        self.emit = emit
    }

    deinit { pending?.cancel() }

    /// Records the grid the connection was opened at. Emits nothing: the
    /// socket already carried this size as a query parameter, and repeating it
    /// as a frame would resize the pane to the size it is.
    public func establish(_ grid: TerminalGrid) {
        pending?.cancel()
        pending = nil
        self.grid = grid
    }

    /// The visible area changed — soft keyboard, rotation, split view. This
    /// never emits. It covers the bottom of a grid whose dimensions did not
    /// change; the surface scrolls to keep the cursor visible.
    public func viewportChanged(to size: CGSize) {
        viewport = size
    }

    /// The surface reported the grid it laid out at. Also never emits:
    /// reporting a grid is not declaring one, and a renderer measuring itself
    /// must not reach the pty.
    public func surfaceReportedGrid(cols: Int, rows: Int) {}

    /// The user deliberately chose a different grid. Emits exactly one resize
    /// once the choice settles.
    public func requestGrid(_ requested: TerminalGrid) {
        guard requested != grid else {
            // Already declared. Cancel any in-flight change back to it so a
            // there-and-back adjustment emits nothing rather than two frames.
            pending?.cancel()
            pending = nil
            return
        }
        pending?.cancel()
        let debounce = debounce
        pending = Task { [weak self] in
            try? await Task.sleep(for: debounce)
            guard !Task.isCancelled, let self else { return }
            self.pending = nil
            self.grid = requested
            self.emittedCount += 1
            self.emit(requested)
        }
    }

    /// Waits for a pending change to land, for tests and for teardown.
    public func settle() async {
        await pending?.value
    }
}
