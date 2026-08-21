import Foundation

/// The position the gateway needs during the WebSocket upgrade.  The server
/// speaks first, using this position to choose a retained mutation batch or a
/// snapshot; clients must not send a synthetic resume frame on every connect.
public struct ConversationResumePosition: Equatable, Sendable {
    public var generation: String?
    public var afterRevision: UInt64?
    public var operationEpoch: String?

    public init(generation: String? = nil, afterRevision: UInt64? = nil, operationEpoch: String? = nil) {
        self.generation = generation
        self.afterRevision = afterRevision
        self.operationEpoch = operationEpoch
    }
}

public enum ConversationSocketState: Equatable, Sendable {
    case idle
    case connecting
    case open
    case reconnecting(attempt: Int)
    case stopped
}

public enum ConversationSocketEvent: Sendable {
    case state(ConversationSocketState)
    case message(ConversationServerMessage)
    case failure(String)
}

public enum ConversationSocketError: Error, Equatable, Sendable {
    case notConnected
    case malformedMessage
}

/// Small seam around URLSession's task so the protocol client is testable
/// without a network listener.  A paired Noise route remains an ordinary
/// loopback WebSocket at this boundary.
public protocol ConversationSocketConnection: Sendable {
    func receive() async throws -> Data
    func send(_ data: Data) async throws
    func cancel()
}

public final class URLSessionConversationSocketConnection: ConversationSocketConnection, @unchecked Sendable {
    private let task: URLSessionWebSocketTask

    public init(task: URLSessionWebSocketTask) {
        self.task = task
        task.resume()
    }

    public func receive() async throws -> Data {
        switch try await task.receive() {
        case .data(let data):
            data
        case .string(let text):
            Data(text.utf8)
        @unknown default:
            throw ConversationSocketError.malformedMessage
        }
    }

    public func send(_ data: Data) async throws {
        try await task.send(.data(data))
    }

    public func cancel() {
        task.cancel(with: .goingAway, reason: nil)
    }
}

/// One reconnecting conversation connection.  It deliberately has no
/// conversation reducer: snapshots and revisioned mutations belong in the
/// per-session store, while this type only owns framing and bounded retry.
public actor ConversationSocket {
    public typealias ConnectionFactory = @Sendable (ConversationResumePosition) async throws -> any ConversationSocketConnection
    public typealias EventHandler = @Sendable (ConversationSocketEvent) async -> Void

    private let makeConnection: ConnectionFactory
    private let eventHandler: EventHandler
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()
    private var resumePosition = ConversationResumePosition()
    private var connection: (any ConversationSocketConnection)?
    private var task: Task<Void, Never>?
    private var shouldRun = false

    public init(makeConnection: @escaping ConnectionFactory, eventHandler: @escaping EventHandler) {
        self.makeConnection = makeConnection
        self.eventHandler = eventHandler
    }

    deinit { task?.cancel() }

    public func start(position: ConversationResumePosition) {
        resumePosition = position
        guard !shouldRun else { return }
        shouldRun = true
        task = Task { await self.run() }
    }

    public func updateResumePosition(_ position: ConversationResumePosition) {
        resumePosition = position
    }

    public func stop() {
        shouldRun = false
        task?.cancel()
        task = nil
        connection?.cancel()
        connection = nil
        Task { await eventHandler(.state(.stopped)) }
    }

    public func send(_ message: ConversationClientMessage) async throws {
        guard let connection else { throw ConversationSocketError.notConnected }
        try await connection.send(encoder.encode(message))
    }

    private func run() async {
        var attempt = 0
        while shouldRun, !Task.isCancelled {
            await eventHandler(.state(attempt == 0 ? .connecting : .reconnecting(attempt: attempt)))
            do {
                let opened = try await makeConnection(resumePosition)
                guard shouldRun, !Task.isCancelled else {
                    opened.cancel()
                    return
                }
                connection = opened
                attempt = 0
                await eventHandler(.state(.open))

                while shouldRun, !Task.isCancelled {
                    let data = try await opened.receive()
                    await eventHandler(.message(try decoder.decode(ConversationServerMessage.self, from: data)))
                }
            } catch is CancellationError {
                return
            } catch {
                connection = nil
                guard shouldRun, !Task.isCancelled else { return }
                attempt += 1
                await eventHandler(.failure(error.localizedDescription))
                let delay = min(8.0, 0.25 * pow(2.0, Double(min(attempt - 1, 5))))
                try? await Task.sleep(for: .seconds(delay))
            }
        }
    }
}
