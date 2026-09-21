import Foundation

// Presentation values for the chat screen. These are not wire models: the
// generated contract stays the only description of what the Hub sends, and
// these types describe what the phone shows. `ConversationProjection` is the
// only place that turns one into the other.
//
// Nothing here may know which agent produced a conversation. Grouping uses
// `parentMessageId`, ordinals, and state — never a connector id or a tool id
// shape a particular connector happens to use.

/// The whole transcript as the screen presents it.
public struct ConversationTranscriptPresentation: Equatable, Sendable {
    public var turns: [ConversationTurnPresentation]
    /// The one request the host is waiting on, if any. It is the same value
    /// that appears in its turn, so the composer and the transcript agree on
    /// which `requestId` an answer targets.
    public var pendingRequest: ConversationRequestPresentation?

    public init(turns: [ConversationTurnPresentation] = [], pendingRequest: ConversationRequestPresentation? = nil) {
        self.turns = turns
        self.pendingRequest = pendingRequest
    }

    public static let empty = ConversationTranscriptPresentation()

    /// The row an item is drawn in. A tool call has no row of its own; it is
    /// drawn inside its activity group. Scroll targets go through this, so a
    /// store anchor that names a tool still lands on something on screen.
    public func rowID(containing itemID: String) -> String? {
        for turn in turns {
            if turn.prompt?.id == itemID { return itemID }
            for entry in turn.entries {
                if entry.id == itemID { return itemID }
                if case .activity(let group) = entry, group.tools.contains(where: { $0.id == itemID }) {
                    return group.id
                }
            }
        }
        return nil
    }

    /// The most recent tool still reported as running, across all visible
    /// turns. Its ordinal is authoritative even when an older turn's activity
    /// group was updated later in the socket stream.
    public var newestRunningTool: ConversationToolPresentation? {
        var newest: ConversationToolPresentation?
        for turn in turns {
            for entry in turn.entries {
                guard case .activity(let group) = entry else { continue }
                for tool in group.tools where tool.status.isRunning {
                    if newest.map({ tool.ordinal > $0.ordinal }) ?? true {
                        newest = tool
                    }
                }
            }
        }
        return newest
    }
}

/// One exchange: the person's message, then everything that followed it until
/// the next one. A window that begins mid-exchange has a turn with no prompt.
///
/// There is no provider turn-completion boundary yet, so a turn only knows
/// whether it is the newest one, never whether it is finished.
public struct ConversationTurnPresentation: Identifiable, Equatable, Sendable {
    public let id: String
    public var prompt: ConversationMessagePresentation?
    public var entries: [ConversationTurnEntry]
    public var isNewest: Bool

    public init(id: String, prompt: ConversationMessagePresentation?, entries: [ConversationTurnEntry], isNewest: Bool) {
        self.id = id
        self.prompt = prompt
        self.entries = entries
        self.isNewest = isNewest
    }
}

public enum ConversationTurnEntry: Identifiable, Equatable, Sendable {
    case message(ConversationMessagePresentation)
    case activity(ConversationActivityGroup)
    case request(ConversationRequestPresentation)
    /// A kind this build cannot present. Shown, not hidden: a gap in the
    /// transcript would be worse than an honest placeholder.
    case unrecognized(id: String, type: String?)

    public var id: String {
        switch self {
        case .message(let message): message.id
        case .activity(let group): group.id
        case .request(let request): request.id
        case .unrecognized(let id, _): id
        }
    }
}

public enum ConversationMessageRole: Equatable, Sendable {
    case user
    case assistant
    /// A role added after this build shipped; presented like the agent's
    /// prose rather than dropped.
    case other(String)
}

public struct ConversationMessagePresentation: Identifiable, Equatable, Sendable {
    public let id: String
    public let ordinal: UInt64
    public let role: ConversationMessageRole
    public let text: String
    public let status: MessageStatus
    /// For a message this phone sent that the host has not yet shown back:
    /// how its delivery stands. Nil for every message the host owns.
    public let pendingDelivery: ConversationOperationStatus?

    public init(
        id: String,
        ordinal: UInt64,
        role: ConversationMessageRole,
        text: String,
        status: MessageStatus,
        pendingDelivery: ConversationOperationStatus? = nil
    ) {
        self.id = id
        self.ordinal = ordinal
        self.role = role
        self.text = text
        self.status = status
        self.pendingDelivery = pendingDelivery
    }

    /// Only states worth a word: an observed message needs no caption.
    public var deliveryCaption: String? {
        switch pendingDelivery {
        case .ambiguous: return "Delivery unknown"
        case .manualReview: return "Not confirmed"
        case .refused: return "Not sent"
        case .sending, nil: break
        }
        switch status {
        case .submitted: return "Sending…"
        case .failed: return "Not delivered"
        case .observed, .partial, .complete: return nil
        }
    }
}

public enum ConversationToolStatus: Equatable, Sendable {
    case running
    case succeeded
    case failed
    /// A status added after this build shipped. Treated as settled: an
    /// unknown value must not keep an activity line spinning forever.
    case other(String)

    public var isRunning: Bool { self == .running }
}

public struct ConversationToolPresentation: Identifiable, Equatable, Sendable {
    public let id: String
    public let ordinal: UInt64
    public let name: String
    public let summary: String
    public let status: ConversationToolStatus

    public init(id: String, ordinal: UInt64, name: String, summary: String, status: ConversationToolStatus) {
        self.id = id
        self.ordinal = ordinal
        self.name = name
        self.summary = summary
        self.status = status
    }
}

/// A run of tool calls presented as one disclosure. Failed calls are statuses
/// inside the run, not separate messages.
public struct ConversationActivityGroup: Identifiable, Equatable, Sendable {
    /// The first call's item id, so the group keeps its identity as calls
    /// are appended to it.
    public let id: String
    /// The message these calls were made from, when the host said so.
    public let parentMessageId: String?
    public internal(set) var tools: [ConversationToolPresentation]

    public init(id: String, parentMessageId: String?, tools: [ConversationToolPresentation]) {
        self.id = id
        self.parentMessageId = parentMessageId
        self.tools = tools
    }

    public var isRunning: Bool { tools.contains { $0.status.isRunning } }

    /// The newest call still running: what the one live activity line names.
    public var runningTool: ConversationToolPresentation? {
        tools.last { $0.status.isRunning }
    }

    public var failedCount: Int { tools.filter { $0.status == .failed }.count }

    /// One concise line: the live call while the run is active, a count once
    /// it has settled.
    public var summary: String {
        if let running = runningTool {
            return "Running \(running.name)"
        }
        let count = tools.count == 1 ? "Ran 1 tool" : "Ran \(tools.count) tools"
        let failed = failedCount
        return failed == 0 ? count : "\(count), \(failed) failed"
    }
}

public enum ConversationRequestKind: Equatable, Sendable {
    case permission
    case question
    case other(String)

    public var title: String {
        switch self {
        case .permission: "Permission requested"
        case .question, .other: "Question"
        }
    }
}

public enum ConversationRequestStatus: Equatable, Sendable {
    case pending
    case resolved
    case dismissed
    case other(String)

    /// Past-tense outcome shown on a settled request row.
    public var outcome: String? {
        switch self {
        case .pending: nil
        case .resolved: "Answered"
        case .dismissed: "Dismissed"
        case .other(let value): value.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}

public struct ConversationRequestPresentation: Identifiable, Equatable, Sendable {
    /// The transcript item id.
    public let id: String
    public let ordinal: UInt64
    /// The id an answer must target. Never inferred from position.
    public let requestId: String
    public let kind: ConversationRequestKind
    public let prompt: String
    public let choices: [String]
    public let status: ConversationRequestStatus
    /// True only for the request the host's state names as pending: an older
    /// row still marked pending is history, not something to answer.
    public let isAwaitingAnswer: Bool
    /// The latest answer this phone gave to this exact request, if any.
    public let answer: ConversationRequestAnswerPresentation?

    public init(
        id: String,
        ordinal: UInt64,
        requestId: String,
        kind: ConversationRequestKind,
        prompt: String,
        choices: [String],
        status: ConversationRequestStatus,
        isAwaitingAnswer: Bool,
        answer: ConversationRequestAnswerPresentation? = nil
    ) {
        self.id = id
        self.ordinal = ordinal
        self.requestId = requestId
        self.kind = kind
        self.prompt = prompt
        self.choices = choices
        self.status = status
        self.isAwaitingAnswer = isAwaitingAnswer
        self.answer = answer
    }
}
