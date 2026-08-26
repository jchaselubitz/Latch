import LatchMobileKit
import SwiftUI

/// The session's real terminal, drawn natively and fed by
/// `WS /v2/sessions/{id}/terminal`.
///
/// The opening order is fixed and it matters: request the preview (which needs
/// only `observe`, so it succeeds on phones that may never attach), paint it,
/// take the attach geometry from its `cols`/`rows`, and only then attach if the
/// route asked for it. The preview and the socket are never in flight together
/// — the geometry for the attach comes from the preview, and issuing them
/// concurrently would mean guessing the size after all.
struct TerminalView: View {
    let session: SessionSummary
    /// Whether arriving here takes the session's exclusive surface from the
    /// Mac immediately. Decided by `SessionRoute.route`, never here.
    let autoAttach: Bool

    @Environment(AppModel.self) private var model

    @State private var preview: PreviewState = .loading
    @State private var terminal: TerminalSession?
    @State private var surface = SwiftTermSurface()
    @State private var coordinator: TerminalGeometryCoordinator?
    @State private var viewport: CGSize = .zero
    @State private var pump: Task<Void, Never>?
    @State private var didOpen = false
    @State private var showStealBanner = false
    /// Set when the device-owner check refused, so the footer can say why the
    /// terminal did not open instead of the button appearing to do nothing.
    @State private var unlockRefusal: String?

    private enum PreviewState {
        case loading
        case loaded(SessionPreview)
        /// The pane could not be read. Not fatal: attaching is still offered,
        /// because a capture that timed out says nothing about the socket.
        case failed(String)

        var value: SessionPreview? {
            if case .loaded(let preview) = self { return preview }
            return nil
        }
    }

    var body: some View {
        GeometryReader { proxy in
            content
                .onAppear { report(viewport: proxy.size) }
                .onChange(of: proxy.size) { _, size in report(viewport: size) }
        }
        .navigationTitle(session.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await loadPreview() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(isAttached)
                .accessibilityLabel("Refresh the still")
            }
            // The default presentation is a default, not a trap: a session that
            // could be a conversation can still be opened as one from here,
            // without changing the setting for every other session.
            if chatPossible {
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink {
                        ChatView(session: session)
                    } label: {
                        Image(systemName: "bubble.left.and.bubble.right")
                    }
                    .accessibilityLabel("Open the conversation instead")
                }
            }
        }
        .task {
            guard !didOpen else { return }
            didOpen = true
            await open()
        }
        // Leaving the screen gives the terminal back. The connection is
        // forgotten rather than merely detached: nothing displays its state
        // once the screen is gone, and a fresh one on re-entry re-reads the
        // pane through the preview anyway.
        .onDisappear {
            pump?.cancel()
            pump = nil
            model.discardTerminal(for: session)
            terminal = nil
        }
        // The user changed the grid in Settings. This is the only input in the
        // whole screen that is allowed to reach the pty as a resize.
        .onChange(of: model.terminalSize) { _, _ in requestGrid() }
        .onChange(of: terminal?.state) { _, state in
            if case .attached = state {
                // The bar arrives with the surface, but the keyboard is not
                // raised: the first thing anyone does on arrival is read, and
                // a keyboard that seizes half the screen unasked would cover
                // the prompt this feature exists to show. A tap on the
                // terminal brings both up.
                surface.installKeyBar()
                if terminal?.stoleSurface == true { flashStealBanner() }
            } else {
                // The bar goes and the keyboard goes with it. A keyboard left
                // standing over a closed screen offers half the keys of a
                // terminal that is no longer there.
                surface.removeKeyBar()
                surface.setFocus(false)
            }
        }
    }

    // MARK: - Screen states

    @ViewBuilder
    private var content: some View {
        switch terminal?.state {
        case .attached:
            attachedScreen
        case .connecting:
            still(over: "Taking the terminal…", dimmed: true, showProgress: true)
        case .failed(let reason):
            closedScreen(title: "The terminal connection failed.", detail: reason)
        case .closed(let reason):
            closedScreen(for: reason)
        case .idle, nil:
            previewScreen
        }
    }

    /// Before anything has been taken. This is the whole point of the preview:
    /// a permission prompt is legible here, at the desk's own geometry, with
    /// the Mac still holding its own terminal.
    @ViewBuilder
    private var previewScreen: some View {
        switch preview {
        case .loading:
            still(over: "Reading the screen…", dimmed: true, showProgress: true)
        case .loaded(let value):
            if autoAttach, session.isRunning {
                // Auto-attach in flight. The still is what the user looks at
                // while the steal happens, not a blank screen.
                still(over: "Taking the terminal…", dimmed: true, showProgress: true)
            } else {
                stillWithFooter(
                    label: capturedLabel(value),
                    detail: unlockRefusal ?? (session.isRunning
                        ? "Attach to type. That takes the terminal from your Mac."
                        : "This session is \(session.state). Attaching may find nothing running."),
                    actionTitle: session.isRunning ? "Attach" : "Attach anyway"
                )
            }
        case .failed(let reason):
            stillWithFooter(
                label: "Could not read the screen.",
                detail: reason,
                actionTitle: "Attach"
            )
        }
    }

    /// No tap gesture of its own: the emulator's view already raises the
    /// keyboard on a tap when it is not first responder, and routes the tap to
    /// selection, mouse reporting, or a semantic-prompt click when it is. A
    /// SwiftUI recognizer layered over that would compete for the same touch
    /// and answer only the first of those cases.
    private var attachedScreen: some View {
        surfaceView
            .overlay(alignment: .top) {
                if showStealBanner {
                    BannerView(text: "Took the terminal from your Mac")
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
    }

    @ViewBuilder
    private func closedScreen(for reason: TerminalCloseReason?) -> some View {
        switch reason {
        case .stolen:
            closedScreen(
                title: "Your Mac took the terminal back.",
                detail: "Something attached to this session elsewhere. Reattaching takes it here again.",
                // Re-read the pane so the screen is not frozen at the last
                // byte this phone happened to receive.
                refreshesPreview: true
            )
        case .sessionExited:
            closedScreen(
                title: "This session's program exited.",
                detail: "There is nothing left to attach to.",
                actionTitle: nil
            )
        case .slowClient:
            closedScreen(
                title: "The connection could not keep up.",
                detail: "The Mac dropped this phone rather than let the session stall behind it."
            )
        case .detached:
            closedScreen(
                title: "Detached.",
                detail: "The Mac has its terminal back."
            )
        case .kernelError:
            closedScreen(
                title: "The session's terminal failed on the Mac.",
                detail: "Check the session on the Mac before reattaching."
            )
        case nil:
            closedScreen(
                title: "The connection closed.",
                detail: "The Mac closed it for a reason this build does not recognise."
            )
        }
    }

    private func closedScreen(
        title: String,
        detail: String,
        actionTitle: String? = "Reattach",
        refreshesPreview: Bool = false
    ) -> some View {
        VStack(spacing: 0) {
            surfaceView
            VStack(spacing: 10) {
                Text(title).font(.headline)
                Text(detail)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                if let actionTitle {
                    Button(actionTitle) { Task { await reattach() } }
                        .buttonStyle(.borderedProminent)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(16)
            .background(.bar)
        }
        .task(id: refreshesPreview) {
            guard refreshesPreview else { return }
            await loadPreview()
        }
    }

    // MARK: - Pieces

    /// The surface, framed at the size its declared grid actually needs and
    /// panned over when that exceeds the viewport.
    ///
    /// The framing is not decoration. A renderer lays out whatever grid fits
    /// its bounds, so a surface merely dropped into the viewport would render
    /// ~59 columns while the pty had been told 100 — the exact mismatch the
    /// geometry rule exists to prevent. Panning is what pays for keeping the
    /// desk's grid.
    private var surfaceView: some View {
        let fontSize = TerminalGeometry.fontSize(cols: grid.cols, viewportWidth: viewport.width)
        let size = TerminalGeometry.pixelSize(cols: grid.cols, rows: grid.rows, fontSize: fontSize)
        let pans = size.width > viewport.width + 1
        return ScrollView(pans ? [.horizontal] : [], showsIndicators: pans) {
            SwiftTermSurfaceView(surface: surface, fontSize: fontSize)
                .frame(width: max(size.width, viewport.width))
                // Deaf until attached. The emulator's view raises the keyboard
                // on a tap of its own accord, and over a still that would be a
                // keyboard with no key bar and nowhere for its keystrokes to
                // go. Panning still works: only the surface stops taking
                // touches, not the scroll view around it.
                .allowsHitTesting(isAttached)
        }
        .background(Color.black)
    }

    private func still(over message: String, dimmed: Bool, showProgress: Bool) -> some View {
        surfaceView
            .overlay {
                if dimmed { Color.black.opacity(0.45) }
            }
            .overlay {
                VStack(spacing: 8) {
                    if showProgress { ProgressView() }
                    Text(message).font(.footnote)
                }
                .padding(14)
                .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 10))
            }
    }

    private func stillWithFooter(
        label: String,
        detail: String,
        actionTitle: String
    ) -> some View {
        VStack(spacing: 0) {
            surfaceView
            VStack(spacing: 8) {
                Text(label).font(.footnote.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(actionTitle) { Task { await reattach() } }
                    .buttonStyle(.borderedProminent)
            }
            .frame(maxWidth: .infinity)
            .padding(14)
            .background(.bar)
        }
    }

    // MARK: - Opening

    /// The fixed order: preview, paint, geometry, then attach if asked.
    private func open() async {
        await loadPreview()
        guard autoAttach, session.isRunning else { return }
        await attach()
    }

    private func loadPreview() async {
        preview = .loading
        do {
            // Scrollback is a courtesy for shells; the gateway ignores it while
            // the alternate screen is up, so asking costs nothing for an agent.
            let value = try await model.previewSession(for: session, scrollbackLines: 200)
            preview = .loaded(value)
            paint(value)
        } catch {
            preview = .failed(Self.message(for: error))
        }
    }

    /// Draws the still.
    ///
    /// `reset()` first, because leftover attributes and cursor position from a
    /// previous paint bleed into a capture, and the cursor is hidden because a
    /// stray block parked wherever the content ended is the only visible
    /// artifact of a still.
    private func paint(_ value: SessionPreview) {
        surface.reset()
        var bytes = Data("\u{1B}[?25l".utf8)
        bytes.append(Data(value.content.utf8))
        surface.feed(bytes)
    }

    /// Takes the surface, after the device owner has confirmed.
    ///
    /// The check comes first because everything below it is a steal: by the
    /// time bytes are flowing the Mac has already lost its terminal.
    private func attach() async {
        guard await model.unlockTerminal() else {
            unlockRefusal = model.terminalUnlockFailure
            return
        }
        unlockRefusal = nil
        guard let terminal = terminalSession() else { return }
        let grid = grid
        // A full reset between the still and the first live byte. The kernel
        // repaints the pane's current frame on attach, and letting that land on
        // top of a preview drawn at a different geometry would interleave two
        // pictures of the same pane.
        surface.reset()
        startPump(terminal)
        coordinator(for: terminal).establish(grid)
        terminal.attach(cols: grid.cols, rows: grid.rows)
    }

    /// Reattaching is another steal, so it is always something the user does.
    private func reattach() async {
        if preview.value == nil { await loadPreview() }
        await attach()
    }

    private func terminalSession() -> TerminalSession? {
        if let terminal { return terminal }
        let created = model.terminalSession(for: session)
        terminal = created
        return created
    }

    /// One consumer of the session's output, for the life of this screen.
    private func startPump(_ terminal: TerminalSession) {
        guard pump == nil else { return }
        let output = terminal.output
        pump = Task { @MainActor in
            for await data in output {
                if Task.isCancelled { return }
                surface.feed(data)
            }
        }
    }

    private func flashStealBanner() {
        withAnimation { showStealBanner = true }
        Task {
            try? await Task.sleep(for: .seconds(3))
            withAnimation { showStealBanner = false }
        }
    }

    // MARK: - Geometry

    /// The grid this screen attaches at. Derived from the preference and the
    /// preview, never from the keyboard or the orientation.
    private var grid: TerminalGrid {
        TerminalGeometry.grid(
            for: model.terminalSize,
            preview: preview.value,
            viewport: viewport
        )
    }

    /// Whether this session could be shown as a conversation instead. It needs
    /// both a connector to drive it and a Hub to talk to; `.unknown` counts,
    /// because an older Mac that omits the field must keep behaving as it does
    /// today.
    private var chatPossible: Bool {
        guard model.surface.chat else { return false }
        switch session.connector {
        case .named, .unknown: return true
        case .none: return false
        }
    }

    private var isAttached: Bool {
        if case .attached = terminal?.state { return true }
        return false
    }

    private func coordinator(for terminal: TerminalSession) -> TerminalGeometryCoordinator {
        if let coordinator { return coordinator }
        let created = TerminalGeometryCoordinator { grid in
            terminal.resize(cols: grid.cols, rows: grid.rows)
        }
        coordinator = created
        return created
    }

    /// The visible area changed — soft keyboard, rotation, split view. It
    /// reaches the pty as nothing at all: it changes the font size and the pan
    /// extent, and the grid is not a function of it.
    private func report(viewport size: CGSize) {
        viewport = size
        coordinator?.viewportChanged(to: size)
    }

    /// A deliberate grid change, and the only path that emits a resize frame.
    private func requestGrid() {
        guard let terminal, isAttached else { return }
        coordinator(for: terminal).requestGrid(grid)
    }

    private static func message(for error: Error) -> String {
        if let error = error as? LatchError {
            switch error {
            case .endpointUnavailable:
                return "This Mac is too old to show a still of the screen."
            case .http(let status, _, let reason):
                return reason.isEmpty ? "The Mac answered \(status)." : reason
            case .transport(let detail):
                return detail
            default:
                break
            }
        }
        return error.localizedDescription
    }
}

/// A still of the pane with nothing behind it — no socket, no keyboard, no way
/// to type. It exists for the screens that explain why this phone cannot
/// attach: an observing phone may *read* the pane, so the screen saying it
/// cannot type can still show what it cannot type at.
struct TerminalStillView: View {
    let session: SessionSummary

    @Environment(AppModel.self) private var model
    @State private var surface = SwiftTermSurface()
    @State private var failure: String?

    var body: some View {
        SwiftTermSurfaceView(surface: surface, fontSize: 11)
            // A still with nothing behind it: it must not raise a keyboard.
            .allowsHitTesting(false)
            .background(Color.black)
            .overlay {
                if let failure {
                    Text(failure)
                        .font(.footnote)
                        .padding(12)
                        .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 10))
                }
            }
            .task {
                do {
                    let preview = try await model.previewSession(for: session)
                    surface.reset()
                    surface.feed(Data("\u{1B}[?25l".utf8) + Data(preview.content.utf8))
                } catch {
                    failure = "Could not read the screen."
                }
            }
    }
}

private extension TerminalView {
    func capturedLabel(_ preview: SessionPreview) -> String {
        "Still of the screen · \(preview.cols)×\(preview.rows)"
    }
}
