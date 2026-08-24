import Foundation
import Observation

public enum TerminalSessionState: Equatable, Sendable {
    case idle
    case connecting
    case attached
    /// The gateway closed the connection. `nil` is a close code this build
    /// does not model.
    case closed(TerminalCloseReason?)
    case failed(String)
}

/// One session's terminal connection, retained by `AppModel`.
///
/// Output is an `AsyncStream` rather than a stored property on purpose: the
/// renderer owns scrollback, and keeping a second copy of a fast agent repaint
/// in an `@Observable` property would invalidate the view per byte. Nothing
/// about the terminal grid is re-rendered by SwiftUI.
@MainActor
@Observable
public final class TerminalSession {
    public private(set) var state: TerminalSessionState = .idle
    /// True while this phone holds the session's single exclusive surface.
    ///
    /// Attaching always takes that surface from whatever held it, so this is
    /// the fact the arrival banner needs: not "something was displaced" but
    /// "the Mac's terminal is now here".
    public private(set) var stoleSurface = false
    /// The grid this connection last declared, so a resize can be skipped when
    /// nothing actually changed.
    public private(set) var cols: Int?
    public private(set) var rows: Int?

    public let sessionID: String

    private let connect: @Sendable (Int, Int) async throws -> any TerminalSocketConnection
    private var socket: TerminalSocket?
    private let stream: AsyncStream<Data>
    private let continuation: AsyncStream<Data>.Continuation

    public var output: AsyncStream<Data> { stream }

    public init(
        sessionID: String,
        connect: @escaping @Sendable (Int, Int) async throws -> any TerminalSocketConnection
    ) {
        self.sessionID = sessionID
        self.connect = connect
        // Buffer rather than drop: a repainting TUI emits faster than a first
        // consumer attaches, and dropping those bytes loses grid state that
        // never repeats.
        var escapee: AsyncStream<Data>.Continuation!
        stream = AsyncStream(bufferingPolicy: .unbounded) { escapee = $0 }
        continuation = escapee
    }

    deinit { continuation.finish() }

    /// Takes the session's surface at the declared grid.
    ///
    /// The size is a parameter and never a guess: it comes from the preview's
    /// reported geometry, so the pane does not resize on attach.
    public func attach(cols: Int, rows: Int) {
        switch state {
        case .connecting, .attached: return
        case .idle, .closed, .failed: break
        }
        self.cols = cols
        self.rows = rows
        let connect = connect
        let socket = TerminalSocket(
            makeConnection: { try await connect(cols, rows) },
            eventHandler: { [weak self] event in await self?.handle(event) }
        )
        self.socket = socket
        state = .connecting
        Task { await socket.start() }
    }

    /// Releases the surface back to the Mac.
    public func detach() {
        guard let socket else { return }
        self.socket = nil
        stoleSurface = false
        state = .closed(.detached)
        Task { await socket.stop() }
    }

    public func send(_ bytes: ArraySlice<UInt8>) {
        guard let socket else { return }
        let data = Data(bytes)
        Task { try? await socket.send(data) }
    }

    /// Declares a new grid. Only a deliberate grid change calls this — the
    /// soft keyboard and rotation must not, because each resize SIGWINCHes the
    /// agent on the Mac and reflows its full-screen TUI.
    public func resize(cols: Int, rows: Int) {
        guard let socket, self.cols != cols || self.rows != rows else { return }
        self.cols = cols
        self.rows = rows
        Task { try? await socket.resize(cols: cols, rows: rows) }
    }

    private func handle(_ event: TerminalSocketEvent) {
        switch event {
        case .connecting:
            state = .connecting
        case .attached:
            state = .attached
            stoleSurface = true
        case .output(let data):
            continuation.yield(data)
        case .closed(let reason, let detail):
            socket = nil
            stoleSurface = false
            state = detail.map(TerminalSessionState.failed) ?? .closed(reason)
        }
    }
}
