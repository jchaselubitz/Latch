import Foundation

/// Wire items to presentation turns. Pure: the same items and state always
/// produce the same turns, so grouping is tested without a view.
///
/// Rules, in the order they apply:
/// - Items are taken in ordinal order; `createdAt` is display metadata only.
/// - Each user message opens a turn. Items before the first user message in
///   the window form a turn without a prompt.
/// - A tool call joins the activity group of the message named by its
///   `parentMessageId` when that message is in the same turn. Otherwise it
///   joins the group immediately before it (ordinal adjacency), or starts one.
///   Any other entry between two parentless calls separates their groups.
/// - A request stays at its ordinal position. It is awaiting an answer only
///   if the host's state names its `requestId` as pending, and it carries the
///   latest answer this phone gave to that exact `requestId`.
/// - A message this phone sent carries its operation's delivery status until
///   the host's own item replaces it.
public enum ConversationProjection {
    public static func project(
        items: [ConversationItem],
        state: ConversationState?,
        operations: [ConversationOperation] = [],
        resolveAttempts: [ConversationResolveAttempt] = []
    ) -> ConversationTranscriptPresentation {
        let ordered = isSortedByOrdinal(items) ? items : items.sorted { $0.ordinal < $1.ordinal }
        let pendingRequestID = state?.pendingRequest
        let deliveries = Dictionary(
            operations.map { ($0.optimisticItemID, $0.status) },
            uniquingKeysWith: { _, newest in newest }
        )
        let answers = Dictionary(
            resolveAttempts.map { ($0.requestId, $0) },
            uniquingKeysWith: { _, newest in newest }
        )

        var turns: [ConversationTurnPresentation] = []
        var builder: TurnBuilder?
        var pending: ConversationRequestPresentation?

        for item in ordered {
            switch item.kind {
            case .message(let role, let text, let status):
                let message = ConversationMessagePresentation(
                    id: item.id,
                    ordinal: item.ordinal,
                    role: messageRole(role),
                    text: text,
                    status: status,
                    pendingDelivery: role == "user" ? deliveries[item.id] : nil
                )
                if role == "user" {
                    if let finished = builder?.build(isNewest: false) { turns.append(finished) }
                    builder = TurnBuilder(prompt: message)
                } else {
                    builder = builder ?? TurnBuilder(prompt: nil, firstID: item.id)
                    builder?.append(message: message)
                }
            case .tool(let name, let summary, let status, let parentMessageId):
                builder = builder ?? TurnBuilder(prompt: nil, firstID: item.id)
                builder?.append(
                    tool: ConversationToolPresentation(
                        id: item.id,
                        ordinal: item.ordinal,
                        name: name,
                        summary: summary,
                        status: toolStatus(status)
                    ),
                    parentMessageId: parentMessageId
                )
            case .request(let requestId, let requestType, let prompt, let choices, let status):
                let request = ConversationRequestPresentation(
                    id: item.id,
                    ordinal: item.ordinal,
                    requestId: requestId,
                    kind: requestKind(requestType),
                    prompt: prompt,
                    choices: choices,
                    status: requestStatus(status),
                    isAwaitingAnswer: status == "pending" && requestId == pendingRequestID,
                    answer: answers[requestId].map(ConversationRequestAnswerPresentation.init(attempt:))
                )
                if request.isAwaitingAnswer { pending = request }
                builder = builder ?? TurnBuilder(prompt: nil, firstID: item.id)
                builder?.append(entry: .request(request))
            case .unrecognized(let type):
                builder = builder ?? TurnBuilder(prompt: nil, firstID: item.id)
                builder?.append(entry: .unrecognized(id: item.id, type: type))
            }
        }
        if let finished = builder?.build(isNewest: true) { turns.append(finished) }
        return ConversationTranscriptPresentation(turns: turns, pendingRequest: pending)
    }

    // The literals below are checked against the canonical schema by
    // `ConversationPresentationContractTests`; an unknown value degrades to
    // `.other` rather than being mistaken for a known one.

    static func messageRole(_ role: String) -> ConversationMessageRole {
        if role == "user" { return .user }
        if role == "assistant" { return .assistant }
        return .other(role)
    }

    static func toolStatus(_ status: String) -> ConversationToolStatus {
        if status == "running" { return .running }
        if status == "succeeded" { return .succeeded }
        if status == "failed" { return .failed }
        return .other(status)
    }

    static func requestKind(_ requestType: String) -> ConversationRequestKind {
        if requestType == "permission" { return .permission }
        if requestType == "question" { return .question }
        return .other(requestType)
    }

    static func requestStatus(_ status: String) -> ConversationRequestStatus {
        if status == "pending" { return .pending }
        if status == "resolved" { return .resolved }
        if status == "dismissed" { return .dismissed }
        return .other(status)
    }

    private static func isSortedByOrdinal(_ items: [ConversationItem]) -> Bool {
        zip(items, items.dropFirst()).allSatisfy { $0.ordinal <= $1.ordinal }
    }
}

private struct TurnBuilder {
    let id: String
    let prompt: ConversationMessagePresentation?
    private(set) var entries: [ConversationTurnEntry] = []
    /// Entry index of each message in this turn, for `parentMessageId`.
    private var messageIndex: [String: Int] = [:]

    init(prompt: ConversationMessagePresentation) {
        id = prompt.id
        self.prompt = prompt
        // The prompt can parent tool calls too; index it before any entry.
        messageIndex[prompt.id] = -1
    }

    init(prompt: ConversationMessagePresentation?, firstID: String) {
        id = firstID
        self.prompt = prompt
    }

    mutating func append(message: ConversationMessagePresentation) {
        messageIndex[message.id] = entries.count
        entries.append(.message(message))
    }

    mutating func append(entry: ConversationTurnEntry) {
        entries.append(entry)
    }

    mutating func append(tool: ConversationToolPresentation, parentMessageId: String?) {
        if let parent = parentMessageId, let parentIndex = messageIndex[parent] {
            // The group belonging to a message sits directly after it.
            let groupIndex = parentIndex + 1
            if groupIndex < entries.count,
               case .activity(var group) = entries[groupIndex],
               group.parentMessageId == parent || group.parentMessageId == nil {
                group.tools.append(tool)
                entries[groupIndex] = .activity(group)
                return
            }
            let group = ConversationActivityGroup(id: tool.id, parentMessageId: parent, tools: [tool])
            entries.insert(.activity(group), at: groupIndex)
            shiftMessageIndex(from: groupIndex)
            return
        }
        if case .activity(var group) = entries.last {
            group.tools.append(tool)
            entries[entries.count - 1] = .activity(group)
            return
        }
        entries.append(.activity(ConversationActivityGroup(id: tool.id, parentMessageId: parentMessageId, tools: [tool])))
    }

    private mutating func shiftMessageIndex(from inserted: Int) {
        for (id, index) in messageIndex where index >= inserted {
            messageIndex[id] = index + 1
        }
    }

    func build(isNewest: Bool) -> ConversationTurnPresentation {
        ConversationTurnPresentation(id: id, prompt: prompt, entries: entries, isNewest: isNewest)
    }
}
