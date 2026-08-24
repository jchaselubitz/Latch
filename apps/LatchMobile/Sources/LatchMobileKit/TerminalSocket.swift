import Foundation

public enum TerminalSocketError: Error, Equatable, Sendable {
    case notConnected
}

/// What one terminal connection reports to its owner.
///
/// There is no `reconnecting` case, and that absence is the design: see
/// `TerminalSocket`.
public enum TerminalSocketEvent: Sendable {
    case connecting
    case attached
    case output(Data)
    /// The connection ended. `reason` is the gateway's application close code
    /// translated through the contract; `detail` carries a transport error
    /// when the socket failed without ever delivering a close frame. Exactly
    /// one of the two is meaningful.
    case closed(reason: TerminalCloseReason?, detail: String?)
}

/// Small seam around URLSession's task so the terminal client is testable
/// without a network listener, matching `ConversationSocketConnection`.
///
/// It carries `closeCode` because a terminal close is information the user
/// must see: `4409` means the Mac took the surface back, which is a different
/// sentence from "the connection dropped".
public protocol TerminalSocketConnection: Sendable {
    func receive() async throws -> Data
    func send(_ bytes: Data) async throws
    /// Control frames go as text; PTY input goes as binary. The gateway
    /// distinguishes them by frame type, never by inspecting bytes.
    func sendControl(_ text: String) async throws
    func cancel()
    /// The close code observed on this connection, once it has closed.
    var closeCode: Int? { get }
}

public final class URLSessionTerminalSocketConnection: TerminalSocketConnection, @unchecked Sendable {
    private let task: URLSessionWebSocketTask

    public init(task: URLSessionWebSocketTask) {
        self.task = task
        task.resume()
    }

    /// `URLSessionWebSocketTask.CloseCode.invalid` has raw value 0 and means
    /// "not closed yet", which is not a code the contract can translate.
    public var closeCode: Int? {
        let raw = task.closeCode.rawValue
        return raw == 0 ? nil : raw
    }

    /// The URL this socket was opened against, including the declared grid.
    public var url: URL? { task.originalRequest?.url }

    public func receive() async throws -> Data {
        switch try await task.receive() {
        case .data(let data):
            data
        case .string(let text):
            // The gateway relays PTY output as binary. A text frame is not
            // expected here, but its bytes are still pane bytes.
            Data(text.utf8)
        @unknown default:
            Data()
        }
    }

    public func send(_ bytes: Data) async throws {
        try await task.send(.data(bytes))
    }

    public func sendControl(_ text: String) async throws {
        try await task.send(.string(text))
    }

    public func cancel() {
        task.cancel(with: .goingAway, reason: nil)
    }
}

/// The resize control frame, spelled as a type so its wire shape is declared
/// in one place rather than interpolated at a call site.
struct TerminalResizeFrame: Codable, Equatable, Sendable {
    var type: String = "resize"
    var cols: Int
    var rows: Int
}

/// One terminal connection. Framing and close translation only.
///
/// **This deliberately diverges from `ConversationSocket`: there is no
/// automatic reconnect.** `ConversationSocket` retries with backoff because
/// reopening a conversation is free. Reopening a terminal is another steal —
/// it takes the session's single exclusive surface away from whoever holds it.
/// Silent retry would let a phone in a pocket repeatedly pull the surface from
/// someone working at the desk. On close this stops and reports why;
/// reattaching is an action a person takes.
public actor TerminalSocket {
    public typealias ConnectionFactory = @Sendable () async throws -> any TerminalSocketConnection
    public typealias EventHandler = @Sendable (TerminalSocketEvent) async -> Void

    private let makeConnection: ConnectionFactory
    private let eventHandler: EventHandler
    private let encoder = JSONEncoder()
    private var connection: (any TerminalSocketConnection)?
    private var task: Task<Void, Never>?
    private var running = false

    public init(makeConnection: @escaping ConnectionFactory, eventHandler: @escaping EventHandler) {
        self.makeConnection = makeConnection
        self.eventHandler = eventHandler
    }

    deinit { task?.cancel() }

    public func start() {
        guard !running else { return }
        running = true
        task = Task { await self.run() }
    }

    /// Deliberate detach. No `closed` event follows: the caller asked for
    /// this, so it already knows the reason.
    public func stop() {
        running = false
        task?.cancel()
        task = nil
        connection?.cancel()
        connection = nil
    }

    public func send(_ bytes: Data) async throws {
        guard let connection else { throw TerminalSocketError.notConnected }
        try await connection.send(bytes)
    }

    public func resize(cols: Int, rows: Int) async throws {
        guard let connection else { throw TerminalSocketError.notConnected }
        let frame = TerminalResizeFrame(cols: cols, rows: rows)
        let encoded = try encoder.encode(frame)
        try await connection.sendControl(String(decoding: encoded, as: UTF8.self))
    }

    private func run() async {
        await eventHandler(.connecting)
        do {
            let opened = try await makeConnection()
            guard running, !Task.isCancelled else {
                opened.cancel()
                return
            }
            connection = opened
            await eventHandler(.attached)
            while running, !Task.isCancelled {
                let data = try await opened.receive()
                if !data.isEmpty {
                    await eventHandler(.output(data))
                }
            }
        } catch is CancellationError {
            return
        } catch {
            // Read the close code before dropping the connection: it is the
            // only place the gateway's reason survives a receive failure.
            let code = connection?.closeCode
            connection = nil
            running = false
            guard !Task.isCancelled else { return }
            await eventHandler(
                .closed(
                    reason: code.flatMap(TerminalCloseReason.forCloseCode),
                    detail: code == nil ? error.localizedDescription : nil
                )
            )
        }
    }
}
