import CoreGraphics
import XCTest

@testable import LatchMobileKit

/// The geometry rule, which exists to make one number small: how many resize
/// frames a phone sends. Each one `SIGWINCH`es the agent on the Mac and
/// reflows its full-screen TUI, so a phone that opened a terminal must not be
/// visibly worse for the session than one that did not.
@MainActor
final class TerminalGeometryTests: XCTestCase {
    private func preview(cols: Int, rows: Int) -> SessionPreview {
        SessionPreview(
            content: "",
            cols: cols,
            rows: rows,
            alternateScreen: true,
            capturedAt: "2026-08-24T09:41:02Z",
            scrollbackLines: 0
        )
    }

    /// Collects what actually went out to the gateway.
    @MainActor
    private final class Recorder {
        var emitted: [TerminalGrid] = []
    }

    /// A short debounce so the suite does not spend 150 ms per case. The app
    /// uses the default.
    private func coordinator(_ recorder: Recorder) -> TerminalGeometryCoordinator {
        TerminalGeometryCoordinator(debounce: .milliseconds(10)) { recorder.emitted.append($0) }
    }

    // MARK: - Choosing the grid

    func testMatchingTheMacTakesTheGridFromThePreviewSoThePaneDoesNotResize() {
        let grid = TerminalGeometry.grid(
            for: .matchMac,
            preview: preview(cols: 100, rows: 30),
            viewport: CGSize(width: 390, height: 700)
        )
        XCTAssertEqual(grid, TerminalGrid(cols: 100, rows: 30))
    }

    func testWithoutAPreviewThereIsNothingToMatchSoItFallsToTheReadableFit() {
        let grid = TerminalGeometry.grid(
            for: .matchMac,
            preview: nil,
            viewport: CGSize(width: 390, height: 700)
        )
        XCTAssertEqual(grid, TerminalGeometry.readableGrid(viewport: CGSize(width: 390, height: 700)))
    }

    func testAFixedChoiceIgnoresBothThePreviewAndTheViewport() {
        for (size, expected) in [
            (TerminalSize.fixed80x24, TerminalGrid(cols: 80, rows: 24)),
            (TerminalSize.fixed100x30, TerminalGrid(cols: 100, rows: 30))
        ] {
            XCTAssertEqual(
                TerminalGeometry.grid(
                    for: size,
                    preview: preview(cols: 132, rows: 43),
                    viewport: CGSize(width: 100, height: 100)
                ),
                expected
            )
        }
    }

    func testTheReadableFitNeverGoesBelowTheFloorOnANarrowPhone() {
        let grid = TerminalGeometry.readableGrid(viewport: CGSize(width: 120, height: 120))
        XCTAssertEqual(grid, TerminalGeometry.minimumGrid)
    }

    /// Font size follows from the grid, not the other way round — and below
    /// the readable floor it stops shrinking, because the surface pans instead.
    func testFontSizeFollowsFromTheGridAndClampsAtTheReadableFloor() {
        let roomy = TerminalGeometry.fontSize(cols: 40, viewportWidth: 390)
        XCTAssertGreaterThan(roomy, TerminalGeometry.readableFontSize)
        let cramped = TerminalGeometry.fontSize(cols: 200, viewportWidth: 390)
        XCTAssertEqual(cramped, TerminalGeometry.readableFontSize)
    }

    /// The declared grid has to be laid out, not merely declared: a renderer
    /// sizes itself to its bounds, so a surface dropped into the viewport
    /// would render ~59 columns while the pty had been told 100.
    func testTheDeclaredGridIsFramedLargerThanTheViewportSoTheSurfacePans() {
        let viewport = CGSize(width: 390, height: 700)
        let fontSize = TerminalGeometry.fontSize(cols: 100, viewportWidth: viewport.width)
        let size = TerminalGeometry.pixelSize(cols: 100, rows: 30, fontSize: fontSize)
        XCTAssertGreaterThan(size.width, viewport.width)
        // Biased upward: rendering a spare column leaves it blank, rendering
        // one too few would clip the pane.
        XCTAssertGreaterThan(
            size.width,
            CGFloat(100) * fontSize * TerminalGeometry.advanceRatio
        )
    }

    // MARK: - When a resize is sent, which is almost never

    /// The soft keyboard and rotation are the two things that change the
    /// visible area constantly. Neither is an input to the pty's geometry.
    func testTheKeyboardAndRotationEmitNoResize() async {
        let recorder = Recorder()
        let coordinator = coordinator(recorder)
        coordinator.establish(TerminalGrid(cols: 100, rows: 30))

        // Portrait, keyboard up, keyboard down, landscape, keyboard up again.
        for size in [
            CGSize(width: 390, height: 780),
            CGSize(width: 390, height: 420),
            CGSize(width: 390, height: 780),
            CGSize(width: 780, height: 390),
            CGSize(width: 780, height: 200)
        ] {
            coordinator.viewportChanged(to: size)
        }
        // The renderer reporting the grid it laid out at is also not a
        // declaration: a surface measuring itself must not reach the pty.
        coordinator.surfaceReportedGrid(cols: 48, rows: 14)
        await coordinator.settle()

        XCTAssertEqual(recorder.emitted, [])
        XCTAssertEqual(coordinator.emittedCount, 0)
        XCTAssertEqual(coordinator.grid, TerminalGrid(cols: 100, rows: 30))
    }

    /// Establishing is not emitting either: the socket already carried the
    /// grid as a query parameter, and repeating it as a frame would resize the
    /// pane to the size it already is.
    func testEstablishingTheOpeningGridSendsNothing() async {
        let recorder = Recorder()
        let coordinator = coordinator(recorder)
        coordinator.establish(TerminalGrid(cols: 100, rows: 30))
        await coordinator.settle()
        XCTAssertEqual(recorder.emitted, [])
    }

    func testADeliberateGridChangeEmitsExactlyOneResizeDebounced() async {
        let recorder = Recorder()
        let coordinator = coordinator(recorder)
        coordinator.establish(TerminalGrid(cols: 100, rows: 30))

        // A person dragging through a picker produces a burst of intermediate
        // values. Only the one they land on may reach the Mac.
        coordinator.requestGrid(TerminalGrid(cols: 80, rows: 24))
        coordinator.requestGrid(TerminalGrid(cols: 60, rows: 20))
        coordinator.requestGrid(TerminalGrid(cols: 90, rows: 26))
        XCTAssertEqual(recorder.emitted, [], "nothing may go out before the choice settles")

        await coordinator.settle()
        XCTAssertEqual(recorder.emitted, [TerminalGrid(cols: 90, rows: 26)])
        XCTAssertEqual(coordinator.emittedCount, 1)
    }

    func testAChangeBackToTheDeclaredGridEmitsNothingAtAll() async {
        let recorder = Recorder()
        let coordinator = coordinator(recorder)
        coordinator.establish(TerminalGrid(cols: 100, rows: 30))

        coordinator.requestGrid(TerminalGrid(cols: 80, rows: 24))
        coordinator.requestGrid(TerminalGrid(cols: 100, rows: 30))
        await coordinator.settle()

        XCTAssertEqual(recorder.emitted, [])
        XCTAssertEqual(coordinator.grid, TerminalGrid(cols: 100, rows: 30))
    }
}
