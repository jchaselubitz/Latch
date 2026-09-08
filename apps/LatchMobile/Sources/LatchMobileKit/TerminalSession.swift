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
    /// The transport dropped while this phone held the surface. `resumable`
    /// says whether a bounded capability exists to take it back without a
    /// steal; otherwise the person must reconnect deliberately.
    case interrupted(resumable: Bool)
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

    /// Bytes were typed at this attach and the transport then dropped without
    /// a close frame. Whether the last keystrokes reached the Mac is unknown,
    /// and they are never replayed.
    public private(set) var inputMayBeUndelivered = false
    /// The gateway's resume capability for the current or interrupted attach,
    /// and when it stops being usable.
    private var resumeCapability: String?
    private var resumeDeadline: Date?
    private var typedSinceAttach = false

    /// When this phone last typed at, resized, or took the terminal.
    ///
    /// Output from the Mac deliberately does not move it. A build printing for
    /// ten minutes is the Mac working, not the person watching, and holding a
    /// session's one surface open on the strength of it is exactly what the
    /// idle release exists to stop.
    public private(set) var lastInputAt: Date

    /// Whether this phone currently holds — or is taking — the session's one
    /// surface. The connecting case counts: the steal is already under way.
    public var holdsSurface: Bool {
        switch state {
        case .connecting, .attached: return true
        case .idle, .closed, .failed, .interrupted: return false
        }
    }

    /// Whether `resume()` can take the surface back without displacing anyone:
    /// the attach was interrupted, the gateway handed out a capability, and
    /// its window has not passed.
    public var canResume: Bool {
        guard case .interrupted(resumable: true) = state, let resumeDeadline else { return false }
        return now() < resumeDeadline
    }

    private let now: @Sendable () -> Date
    private let connect: @Sendable (Int, Int, String?) async throws -> any TerminalSocketConnection
    private var socket: TerminalSocket?
    private let stream: AsyncStream<Data>
    private let continuation: AsyncStream<Data>.Continuation

    public var output: AsyncStream<Data> { stream }

    public convenience init(
        sessionID: String,
        now: @escaping @Sendable () -> Date = { Date() },
        connect: @escaping @Sendable (Int, Int) async throws -> any TerminalSocketConnection
    ) {
        self.init(sessionID: sessionID, now: now, resumingConnect: { cols, rows, _ in try await connect(cols, rows) })
    }

    /// The resume-aware form: the third argument is the capability to present
    /// as the `resume` query, or nil for an ordinary attach.
    public init(
        sessionID: String,
        now: @escaping @Sendable () -> Date = { Date() },
        resumingConnect: @escaping @Sendable (Int, Int, String?) async throws -> any TerminalSocketConnection
    ) {
        self.sessionID = sessionID
        self.now = now
        self.lastInputAt = now()
        self.connect = resumingConnect
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
        open(cols: cols, rows: rows, resume: nil)
    }

    /// Takes the surface back after transport loss using the gateway's
    /// capability. The gateway honours it only while unexpired and only if no
    /// other surface attached meanwhile; a refusal arrives as
    /// `.closed(.resumeRefused)` and never steals. Returns false when there
    /// is nothing to resume with.
    @discardableResult
    public func resume() -> Bool {
        guard canResume, let capability = resumeCapability, let cols, let rows else { return false }
        open(cols: cols, rows: rows, resume: capability)
        return true
    }

    private func open(cols: Int, rows: Int, resume: String?) {
        switch state {
        case .connecting, .attached: return
        case .idle, .closed, .failed, .interrupted: break
        }
        self.cols = cols
        self.rows = rows
        lastInputAt = now()
        typedSinceAttach = false
        inputMayBeUndelivered = false
        if resume == nil {
            resumeCapability = nil
            resumeDeadline = nil
        }
        let connect = connect
        let socket = TerminalSocket(
            makeConnection: { try await connect(cols, rows, resume) },
            eventHandler: { [weak self] event in await self?.handle(event) }
        )
        self.socket = socket
        state = .connecting
        Task { await socket.start() }
    }

    /// Releases the surface back to the Mac. A deliberate detach also gives
    /// up any resume capability: coming back is a new decision.
    public func detach() {
        resumeCapability = nil
        resumeDeadline = nil
        guard let socket else {
            if case .interrupted = state { state = .closed(.detached) }
            return
        }
        self.socket = nil
        stoleSurface = false
        state = .closed(.detached)
        Task { await socket.stop() }
    }

    /// Sends input to the held surface. Input typed while the surface is not
    /// held is dropped, never queued: replaying it into whatever the phone
    /// attaches to next would type into a pane nobody chose.
    public func send(_ bytes: ArraySlice<UInt8>) {
        guard let socket, case .attached = state else {
            inputMayBeUndelivered = true
            return
        }
        lastInputAt = now()
        typedSinceAttach = true
        let data = Data(bytes)
        Task { try? await socket.send(data) }
    }

    /// Clears the undelivered-input notice once the person has seen it.
    public func acknowledgeUndeliveredInput() {
        inputMayBeUndelivered = false
    }

    /// The link underneath this surface was lost before the socket noticed.
    /// The socket is dropped now rather than left to time out, the surface
    /// becomes interrupted (resumable only with a live capability), and any
    /// input since attach is reported as possibly undelivered.
    public func interrupt() {
        guard holdsSurface else { return }
        let socket = self.socket
        self.socket = nil
        stoleSurface = false
        if typedSinceAttach { inputMayBeUndelivered = true }
        if let resumeCapability, let resumeWindow, !resumeCapability.isEmpty {
            resumeDeadline = now().addingTimeInterval(resumeWindow)
            state = .interrupted(resumable: true)
        } else {
            state = .interrupted(resumable: false)
        }
        if let socket { Task { await socket.stop() } }
    }

    /// Declares a new grid. Only a deliberate grid change calls this — the
    /// soft keyboard and rotation must not, because each resize SIGWINCHes the
    /// agent on the Mac and reflows its full-screen TUI.
    public func resize(cols: Int, rows: Int) {
        guard let socket, self.cols != cols || self.rows != rows else { return }
        self.cols = cols
        self.rows = rows
        lastInputAt = now()
        Task { try? await socket.resize(cols: cols, rows: rows) }
    }

    private func handle(_ event: TerminalSocketEvent) {
        switch event {
        case .connecting:
            state = .connecting
        case .attached(let capability, let window):
            state = .attached
            stoleSurface = true
            resumeCapability = capability
            resumeDeadline = nil
            resumeWindow = window.map { TimeInterval($0) }
        case .output(let data):
            continuation.yield(data)
        case .closed(let reason, let detail):
            socket = nil
            stoleSurface = false
            switch (reason, detail) {
            case (nil, .some):
                // No close frame: the transport dropped underneath a held
                // surface. Anything typed since attach may or may not have
                // arrived, and it will not be sent again.
                if typedSinceAttach { inputMayBeUndelivered = true }
                if let resumeCapability, let resumeWindow, !resumeCapability.isEmpty {
                    resumeDeadline = now().addingTimeInterval(resumeWindow)
                    state = .interrupted(resumable: true)
                } else {
                    state = .interrupted(resumable: false)
                }
            case (.some(let reason), _):
                resumeCapability = nil
                resumeDeadline = nil
                state = .closed(reason)
            case (nil, nil):
                resumeCapability = nil
                resumeDeadline = nil
                state = .closed(nil)
            }
        }
    }

    private var resumeWindow: TimeInterval?
}
