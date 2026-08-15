import Foundation
import Observation

/// One session's chat screen.
///
/// It owns the event subscription, the transcript folded out of it, and the
/// send path. The composer's enabled state comes from two places on purpose:
/// discovery says whether the gateway has a `send` endpoint at all, and the
/// per-session `canSend` preflight says whether input is safe right now. The
/// preflight is racy, so a send is always attempted and a 409 is the authority.
@MainActor
@Observable
public final class ChatModel {
    public private(set) var transcript = Transcript()
    public private(set) var streamState: EventStreamState = .connecting
    public private(set) var capabilities: SessionCapabilities?
    public private(set) var error: String?
    /// Set when the stream stopped for a reason no retry fixes.
    public private(set) var endedReason: String?
    public private(set) var isSending = false

    public let session: SessionSummary
    private let gateway: LatchGateway
    private let surface: SessionSurface
    private var stream: EventStream?
    private var pump: Task<Void, Never>?

    public init(session: SessionSummary, gateway: LatchGateway, surface: SessionSurface) {
        self.session = session
        self.gateway = gateway
        self.surface = surface
    }

    /// Whether the composer should accept text.
    public var canCompose: Bool {
        guard surface.composer else { return false }
        guard let capabilities else { return session.isRunning }
        return capabilities.interaction.sendMessage && capabilities.interaction.canSend.ok
    }

    /// Why the composer is disabled, when it is.
    public var composerReason: String? {
        if !surface.composer {
            return "This gateway does not accept messages."
        }
        if let capabilities, !capabilities.interaction.sendMessage {
            return "This session's harness does not accept messages."
        }
        if let reason = capabilities?.interaction.canSend.reason,
           capabilities?.interaction.canSend.ok == false {
            return reason
        }
        return nil
    }

    /// Whether the app may offer buttons for a pending permission or question.
    public var canResolve: Bool {
        guard surface.interactionControls else { return false }
        return capabilities?.interaction.resolve ?? false
    }

    /// Starts the subscription and the capability preflight.
    public func start() async {
        guard pump == nil else { return }
        await refreshCapabilities()
        do {
            let stream = try await gateway.events(sessionId: session.id)
            self.stream = stream
            pump = Task { [weak self] in
                for await message in stream.messages {
                    self?.receive(message)
                }
            }
        } catch let error as LatchError {
            self.error = error.message
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Tears the subscription down when the screen goes away.
    public func stop() {
        pump?.cancel()
        pump = nil
        stream?.close()
        stream = nil
    }

    /// Re-runs the per-session preflight.
    public func refreshCapabilities() async {
        guard surface.interactionControls || surface.composer else { return }
        do {
            capabilities = try await gateway.sessionCapabilities(sessionId: session.id)
        } catch LatchError.endpointUnavailable {
            // Not an error: an older gateway simply cannot answer this, and
            // the surface already reflects that.
            capabilities = nil
        } catch let error as LatchError {
            self.error = error.message
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Sends a message the person typed.
    public func send(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isSending else { return }
        isSending = true
        defer { isSending = false }
        // The sent message is not echoed locally. It comes back through the
        // ledger as a `user_message`, so the transcript only ever shows what
        // the session actually received.
        await submit(.message(trimmed))
    }

    /// Answers a pending permission or question.
    public func resolve(requestId: String, choice: String) async {
        guard canResolve, !isSending else { return }
        isSending = true
        defer { isSending = false }
        transcript.markAnswered(requestId: requestId, choice: choice)
        await submit(.resolve(requestId: requestId, choice: choice))
    }

    private func submit(_ operation: SendOperation) async {
        do {
            _ = try await gateway.send(sessionId: session.id, operation: operation)
            error = nil
        } catch let error as LatchError {
            self.error = error.message
        } catch {
            self.error = error.localizedDescription
        }
        // The screen state the harness reports changes with every submission,
        // so the preflight is refreshed rather than left stale.
        await refreshCapabilities()
    }

    private func receive(_ message: EventStreamMessage) {
        switch message {
        case .state(let state):
            streamState = state
        case .event(let event):
            transcript.apply(event)
        case .resync:
            // The ledger this transcript was built from is gone. Rebuilding
            // from the replay is the only correct answer; merging would
            // interleave two different timelines.
            transcript.reset()
        case .closed(let code, let reason):
            streamState = .closed
            endedReason = Self.describe(code: code, reason: reason)
        }
    }

    private static func describe(code: Int, reason: String) -> String {
        switch code {
        case EventStreamClose.sessionNotFound:
            return "This session is no longer on the computer."
        case EventStreamClose.noConnector:
            return "This session has no agent attached, so there is nothing to watch."
        case EventStreamClose.policy:
            return "The gateway closed the stream: \(reason.isEmpty ? "not permitted" : reason)"
        case EventStreamClose.normal:
            return "The gateway closed the stream."
        default:
            return reason.isEmpty ? "The stream ended (\(code))." : reason
        }
    }
}
