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

/// The size of one terminal cell at a given point size, in points.
///
/// Fitting a grid to a screen is only as good as this number. The first cut
/// used a flat `0.6 × size` advance; the system monospaced face is really
/// `0.618`, and SwiftTerm snaps every cell to the pixel grid on top of that,
/// so every "fitted" grid rendered a few percent wider than the screen and the
/// last columns ran off the right edge. The app measures the renderer's own
/// font; `systemMonospaced` reproduces the same arithmetic for tests.
public struct TerminalCellMetrics: Sendable {
    private let measure: @Sendable (CGFloat) -> CGSize

    public init(_ measure: @escaping @Sendable (CGFloat) -> CGSize) {
        self.measure = measure
    }

    public func cell(atFontSize fontSize: CGFloat) -> CGSize {
        measure(fontSize)
    }

    /// The system monospaced face's own ratios — `W`'s advance, and ascent +
    /// descent + leading — snapped to `scale` the way SwiftTerm snaps them:
    /// width to the nearest device pixel, height up to the next one.
    public static func systemMonospaced(scale: CGFloat = 3) -> TerminalCellMetrics {
        TerminalCellMetrics { fontSize in
            let width = fontSize * 0.6181640625
            let height = ceil(fontSize * 1.177734375)
            return CGSize(
                width: max(1, (width * scale).rounded() / scale),
                height: max(1, ceil(height * scale) / scale)
            )
        }
    }
}

/// The geometry rule: the phone chooses the grid, and by default chooses one
/// that fits its own screen.
///
/// `cols` and `rows` are query parameters on the terminal socket, so the grid
/// is a choice rather than a measurement. A terminal cannot wrap text on the
/// client: a full-screen TUI addresses absolute cells, so the only way to get
/// lines that end at the right edge of a phone is to tell the pty a grid that
/// fits the phone and let the program on the other end reflow itself. That
/// is `readable`, the default. `matchMac` keeps the desk's grid so nothing on
/// the Mac reflows, and renders it at whatever font size fits, panning when
/// even the smallest one does not.
public enum TerminalGeometry {
    /// The point size a fitted grid is chosen at.
    public static let readableFontSize: CGFloat = 11
    /// Below this the font stops shrinking and the surface pans instead.
    public static let minimumFontSize: CGFloat = 9
    /// The smallest grid the readable fit will settle for. A screen too narrow
    /// for this many columns at the readable size gets a slightly smaller font
    /// rather than a grid no full-screen TUI is laid out for.
    public static let minimumGrid = TerminalGrid(cols: 60, rows: 20)

    /// The grid to attach at.
    ///
    /// `matchMac` takes the preview's reported geometry: attaching at the size
    /// the pane already has means the pane does not resize at all — no
    /// `SIGWINCH`, no reflow, and a paused prompt that cannot repaint
    /// transfers exactly as it stands. When there is no preview there is
    /// nothing to match, so it falls through to the readable fit rather than
    /// guessing.
    public static func grid(
        for size: TerminalSize,
        preview: SessionPreview?,
        viewport: CGSize,
        metrics: TerminalCellMetrics = .systemMonospaced()
    ) -> TerminalGrid {
        if let fixed = size.fixedGrid {
            return TerminalGrid(cols: fixed.cols, rows: fixed.rows)
        }
        if size == .matchMac, let preview, preview.cols > 0, preview.rows > 0 {
            return TerminalGrid(cols: preview.cols, rows: preview.rows)
        }
        return readableGrid(viewport: viewport, metrics: metrics)
    }

    /// The largest grid that fits the viewport at the readable font size,
    /// never smaller than `minimumGrid`.
    ///
    /// Rows are counted at the size the columns will actually render at,
    /// which is below the readable size only when the minimum forced extra
    /// columns onto a narrow screen.
    public static func readableGrid(
        viewport: CGSize,
        metrics: TerminalCellMetrics = .systemMonospaced()
    ) -> TerminalGrid {
        let readable = metrics.cell(atFontSize: readableFontSize)
        let fitted = viewport.width > 0 ? Int(viewport.width / readable.width) : 0
        let cols = max(fitted, minimumGrid.cols)
        let rendered = metrics.cell(atFontSize: fontSize(cols: cols, viewportWidth: viewport.width, metrics: metrics))
        let rows = viewport.height > 0 ? Int(viewport.height / rendered.height) : 0
        return TerminalGrid(cols: cols, rows: max(rows, minimumGrid.rows))
    }

    /// Whether `grid` renders without panning sideways in a viewport this wide.
    public static func fitsWidth(
        _ grid: TerminalGrid,
        viewportWidth: CGFloat,
        metrics: TerminalCellMetrics = .systemMonospaced()
    ) -> Bool {
        let size = fontSize(cols: grid.cols, viewportWidth: viewportWidth, metrics: metrics)
        return CGFloat(grid.cols) * metrics.cell(atFontSize: size).width <= viewportWidth
    }

    /// Point size for a chosen grid in a given viewport: the largest size, in
    /// quarter points, whose real cells put every column on screen.
    ///
    /// Font size follows from the grid, not the other way round. Below
    /// `minimumFontSize` it stops shrinking and the caller pans horizontally
    /// instead, which is why this clamps rather than returning a hairline.
    public static func fontSize(
        cols: Int,
        viewportWidth: CGFloat,
        metrics: TerminalCellMetrics = .systemMonospaced()
    ) -> CGFloat {
        guard cols > 0, viewportWidth > 0 else { return readableFontSize }
        // Snapping moves a cell by up to half a device pixel either way, so a
        // size a little above the face's unsnapped ratio can still fit — 11 pt
        // snaps narrower than 10.75 pt at 3x. Start a point above the ratio's
        // guess and come down a quarter point at a time until the snapped
        // cells really fit; that is a handful of steps.
        let ratio = metrics.cell(atFontSize: 100).width / 100
        var size = ((viewportWidth / (CGFloat(cols) * ratio) + 1) * 4).rounded(.down) / 4
        while size > minimumFontSize,
              CGFloat(cols) * metrics.cell(atFontSize: size).width > viewportWidth {
            size -= 0.25
        }
        return max(size, minimumFontSize)
    }
}

/// Decides when a resize frame is sent, and it is almost never.
///
/// Each resize `SIGWINCH`es the agent on the Mac and reflows its full-screen
/// TUI, so the viewport is deliberately not an input to the pty's geometry: a
/// phone that opened a terminal must not be visibly worse for the session than
/// one that did not. The keyboard appearing, the device rotating, and the
/// surface reporting the grid it settled on all reach the pty as nothing at
/// all. Only a deliberate grid change — the Settings control, or the
/// terminal screen's "Fit to Screen" action — emits, and that one is
/// debounced.
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
