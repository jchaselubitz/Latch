import Foundation

/// WebSocket close codes `latch serve` uses to say something specific.
public enum EventStreamClose {
    /// Normal closure.
    public static let normal = 1000
    /// Policy rejection, such as a bad token.
    public static let policy = 1008
    /// The session does not exist. Retrying will not create it.
    public static let sessionNotFound = 4404
    /// The session has no harness connector, so it can never produce events.
    public static let noConnector = 4408
    /// The supplied cursor is no longer valid for this ledger.
    public static let staleCursor = 4422

    /// Codes where reconnecting cannot help.
    static let fatal: Set<Int> = [normal, policy, sessionNotFound, noConnector]
}

/// Connection state of one event subscription.
public enum EventStreamState: String, Sendable {
    case connecting
    case open
    case reconnecting
    case closed
}

/// One thing the subscription tells its consumer.
public enum EventStreamMessage: Sendable {
    case state(EventStreamState)
    case event(HarnessEvent)
    /// The timeline this consumer holds is no longer valid and the stream is
    /// replaying from the beginning. Drop the transcript before the next batch.
    case resync
    /// The stream stopped for good.
    case closed(code: Int, reason: String)
}

/// Retry pacing for a dropped stream.
public struct RetryPolicy: Sendable {
    public let initial: Duration
    public let max: Duration
    public let multiplier: Double

    public init(
        initial: Duration = .milliseconds(250),
        max: Duration = .seconds(10),
        multiplier: Double = 2
    ) {
        self.initial = initial
        self.max = max
        self.multiplier = multiplier
    }

    /// Delay before attempt number `attempt`, counting from 1.
    public func delay(attempt: Int) -> Duration {
        guard attempt > 1 else { return initial }
        let scale = pow(multiplier, Double(attempt - 1))
        let scaled = Double(initial.components.seconds) * scale
            + Double(initial.components.attoseconds) * scale / 1e18
        let capped = Swift.min(
            scaled,
            Double(max.components.seconds) + Double(max.components.attoseconds) / 1e18
        )
        return .milliseconds(Int(capped * 1000))
    }
}

/// A live subscription to one session's harness events.
///
/// The gateway replays from a cursor, so a reconnect resumes where the phone
/// left off rather than duplicating the transcript. Two things invalidate a
/// cursor and force a full replay: the gateway rejecting it as stale, and the
/// connector epoch changing, which means the ledger was rebuilt and the old
/// indexes point at a different timeline.
public final class EventStream: Sendable {
    private let driver: Driver

    /// Everything the subscription reports, in order.
    public let messages: AsyncStream<EventStreamMessage>

    init(
        link: GatewayLink,
        sessionId: String,
        cursor: Int,
        connectorEpoch: Int?,
        session: URLSession,
        retry: RetryPolicy = RetryPolicy()
    ) {
        var sink: AsyncStream<EventStreamMessage>.Continuation!
        messages = AsyncStream { sink = $0 }
        driver = Driver(
            link: link,
            sessionId: sessionId,
            cursor: cursor,
            connectorEpoch: connectorEpoch,
            session: session,
            retry: retry,
            output: sink
        )
        Task { await driver.run() }
    }

    /// Stops the subscription. Safe to call more than once.
    public func close() {
        Task { await driver.stop() }
    }

    /// The cursor a later subscription should resume from.
    public var cursor: Int {
        get async { await driver.cursor }
    }
}

/// The reconnect loop behind `EventStream`.
private actor Driver {
    private let link: GatewayLink
    private let sessionId: String
    private let session: URLSession
    private let retry: RetryPolicy
    private let output: AsyncStream<EventStreamMessage>.Continuation

    private var task: URLSessionWebSocketTask?
    private var stopped = false
    private var attempt = 0
    private var state: EventStreamState = .connecting

    private(set) var cursor: Int
    private var epoch: Int?
    /// True while the cursor is known to be 0 and nothing has arrived since,
    /// so a stale-cursor close cannot be answered by resyncing again.
    private var replayedFromZero: Bool

    init(
        link: GatewayLink,
        sessionId: String,
        cursor: Int,
        connectorEpoch: Int?,
        session: URLSession,
        retry: RetryPolicy,
        output: AsyncStream<EventStreamMessage>.Continuation
    ) {
        self.link = link
        self.sessionId = sessionId
        self.cursor = cursor
        self.epoch = connectorEpoch
        self.session = session
        self.retry = retry
        self.output = output
        self.replayedFromZero = cursor == 0
    }

    func run() async {
        while !stopped {
            let outcome = await connectAndReceive()
            if stopped { break }
            switch outcome {
            case .resync:
                // The cursor is already reset; reconnect immediately rather
                // than waiting out a backoff for a decision we made.
                attempt = 0
                continue
            case .fatal(let code, let reason):
                stopped = true
                emit(.closed(code: code, reason: reason))
                setState(.closed)
                output.finish()
                return
            case .retryable:
                setState(.reconnecting)
                attempt += 1
                do {
                    try await Task.sleep(for: retry.delay(attempt: attempt))
                } catch {
                    // The only thing that cancels this sleep is the driver
                    // going away. Reconnecting after that would spin, so the
                    // loop ends rather than retrying immediately.
                    stopped = true
                }
            }
        }
        setState(.closed)
        output.finish()
    }

    func stop() {
        guard !stopped else { return }
        stopped = true
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        setState(.closed)
        output.finish()
    }

    private enum Outcome {
        case resync
        case retryable
        case fatal(code: Int, reason: String)
    }

    private func connectAndReceive() async -> Outcome {
        guard let url = socketURL() else {
            return .fatal(code: EventStreamClose.policy, reason: "invalid gateway address")
        }
        var request = URLRequest(url: url)
        // The gateway accepts the token either as a bearer header or as the
        // `latch.v1.<token>` subprotocol, and echoes the subprotocol back on
        // the handshake. Both are sent: the header is what survives a proxy
        // that drops subprotocol negotiation.
        request.setValue("Bearer \(link.token)", forHTTPHeaderField: "Authorization")
        request.setValue(
            "latch.v1.\(link.token)",
            forHTTPHeaderField: "Sec-WebSocket-Protocol"
        )
        let socket = session.webSocketTask(with: request)
        task = socket
        setState(attempt == 0 ? .connecting : .reconnecting)
        socket.resume()

        while !stopped {
            do {
                let message = try await socket.receive()
                if state != .open {
                    attempt = 0
                    setState(.open)
                }
                guard case .string(let text) = message else { continue }
                if let outcome = handle(text: text) {
                    socket.cancel(with: .goingAway, reason: nil)
                    task = nil
                    return outcome
                }
            } catch {
                let code = socket.closeCode.rawValue
                let reason = socket.closeReason
                    .flatMap { String(data: $0, encoding: .utf8) } ?? ""
                task = nil
                if stopped {
                    return .retryable
                }
                // A stale cursor is answerable exactly once: reset and replay
                // the whole timeline. Being told it is stale again at cursor 0
                // means something else is wrong, and retrying would spin.
                if code == EventStreamClose.staleCursor, !replayedFromZero {
                    resync()
                    return .resync
                }
                if EventStreamClose.fatal.contains(code) {
                    return .fatal(code: code, reason: reason)
                }
                return .retryable
            }
        }
        return .retryable
    }

    /// Handles one frame. Returns an outcome when the socket must be torn down.
    private func handle(text: String) -> Outcome? {
        guard let data = text.data(using: .utf8),
              let event = try? JSONDecoder().decode(HarnessEvent.self, from: data)
        else {
            // A frame this build cannot parse at all is skipped rather than
            // treated as a disconnect; the contract allows additive fields.
            return nil
        }
        // A different epoch means the ledger was rebuilt, so every cursor the
        // phone holds points into a timeline that no longer exists.
        if let epoch, event.connectorEpoch != epoch, cursor > 0 {
            resync()
            return .resync
        }
        epoch = event.connectorEpoch
        cursor += 1
        replayedFromZero = false
        emit(.event(event))
        return nil
    }

    private func resync() {
        cursor = 0
        epoch = nil
        replayedFromZero = true
        emit(.resync)
    }

    private func socketURL() -> URL? {
        guard var components = URLComponents(
            url: link.url,
            resolvingAgainstBaseURL: false
        ) else { return nil }
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        let escaped = sessionId
            .addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? sessionId
        components.path = "/v1/sessions/\(escaped)/events"
        components.queryItems = [URLQueryItem(name: "cursor", value: String(cursor))]
        return components.url
    }

    private func setState(_ next: EventStreamState) {
        guard state != next else { return }
        state = next
        emit(.state(next))
    }

    private func emit(_ message: EventStreamMessage) {
        output.yield(message)
    }
}
