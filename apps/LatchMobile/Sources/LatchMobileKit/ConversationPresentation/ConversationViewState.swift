import Foundation

/// Whether the Chat screen has a confirmed answer about its conversation.
///
/// Socket open only says the WebSocket handshake completed. Its initial
/// snapshot is delivered separately, so treating `state?.connector == nil`
/// as unsupported confuses a brief, normal loading interval with a refusal.
public enum ConversationSupport: Equatable, Sendable {
    /// No state frame has arrived yet.
    case loading
    /// A state frame arrived and the Hub has not refused this conversation.
    case availableOrStarting
    /// The Hub explicitly reported that it cannot provide this conversation.
    case unavailable

    public static func derive(state: ConversationState?) -> ConversationSupport {
        guard let state else { return .loading }
        return state.phase == "unavailable" ? .unavailable : .availableOrStarting
    }
}

/// The one state the chat screen is in, derived from the store's facts.
///
/// Labels describe what the host reported, not what the agent might be doing:
/// a quiet network is not "thinking", and a quiet turn is not "complete" —
/// the host has no turn-completion boundary to say so.
public enum ConversationViewState: String, CaseIterable, Equatable, Sendable {
    /// No conversation state has arrived yet.
    case loading
    /// The Hub recognized the agent but is waiting for its first source record.
    /// The first message can be accepted in this state.
    case starting
    /// Connected, idle, and nothing has been said.
    case empty
    /// Connected and idle.
    case ready
    /// The host reports the agent is working.
    case working
    /// The host is waiting on a request.
    case awaitingInput
    /// The agent process has exited; the transcript remains.
    case interrupted
    /// The socket dropped and is being re-established.
    case disconnected
    /// The host cannot provide this conversation.
    case failed

    public static func derive(
        socketState: ConversationSocketState,
        connectionError: String?,
        state: ConversationState?,
        hasItems: Bool
    ) -> ConversationViewState {
        if state?.phase == "unavailable" { return .failed }
        switch socketState {
        case .stopped where connectionError != nil:
            return .failed
        case .reconnecting:
            return state == nil && !hasItems ? .loading : .disconnected
        case .idle, .connecting, .open, .stopped:
            if connectionError != nil, state != nil || hasItems { return .disconnected }
        }
        guard let state else { return .loading }
        if state.pendingRequest != nil || state.phase == "awaiting_input" { return .awaitingInput }
        if state.phase == "working" { return .working }
        if state.phase == "exited" { return .interrupted }
        if state.phase == "starting" { return .starting }
        return hasItems ? .ready : .empty
    }

    /// Short status for the toolbar; nil when there is nothing to say.
    public var label: String? {
        switch self {
        case .loading: "Connecting…"
        case .starting: "Starting…"
        case .empty, .ready: nil
        case .working: "Working"
        case .awaitingInput: "Waiting for your answer"
        case .interrupted: "Agent exited"
        case .disconnected: "Reconnecting…"
        case .failed: "Conversation unavailable"
        }
    }

    /// The concise live status for the transcript chrome. The Hub's phase is
    /// still the source of truth; a running tool simply makes "Working" more
    /// useful by naming the newest observed activity. Silence never becomes
    /// a claim that the agent is thinking or that the turn completed.
    public func statusLine(in transcript: ConversationTranscriptPresentation) -> String? {
        guard self == .working else { return label }
        guard let tool = transcript.newestRunningTool else { return label }
        return "Working · Running \(tool.name)"
    }
}
