import Foundation

/// One row in the chat transcript.
public struct TranscriptEntry: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case user(String)
        case assistant(String)
        case tool(name: String, detail: String, finished: Bool)
        case status(String)
        /// A pending permission or question. It stays in the transcript after
        /// it is answered so the conversation reads in order.
        case request(Request)
        /// An event type this build does not know. Shown rather than hidden,
        /// because silently dropping it would misrepresent what the agent did.
        case unrecognized(String)
    }

    public struct Request: Equatable, Sendable {
        public let requestId: String
        public let kind: AwaitingInputKind?
        public let prompt: String
        public let choices: [String]
        /// The choice that was submitted from this device, once one was.
        public var answered: String?

        public init(
            requestId: String,
            kind: AwaitingInputKind?,
            prompt: String,
            choices: [String],
            answered: String? = nil
        ) {
            self.requestId = requestId
            self.kind = kind
            self.prompt = prompt
            self.choices = choices
            self.answered = answered
        }
    }

    public let id: Int
    public let at: String
    public var kind: Kind

    public init(id: Int, at: String, kind: Kind) {
        self.id = id
        self.at = at
        self.kind = kind
    }
}

/// Folds a harness event stream into chat rows.
///
/// The gateway replays from a ledger index, so the reducer is deliberately
/// order-dependent and idempotent only through the cursor: `reset()` on a
/// resync throws the timeline away rather than trying to merge two of them.
public struct Transcript: Equatable, Sendable {
    public private(set) var entries: [TranscriptEntry] = []
    /// The request the agent is currently blocked on, if any.
    public private(set) var pendingRequest: TranscriptEntry.Request?
    /// The most recent harness status, for the session header.
    public private(set) var status: String?

    private var nextId = 0
    /// Index of the row that streaming assistant text is being appended to.
    private var streamingIndex: Int?

    public init() {}

    /// Drops everything, for a replay from cursor 0.
    public mutating func reset() {
        entries = []
        pendingRequest = nil
        status = nil
        streamingIndex = nil
        // Ids keep climbing so SwiftUI never reuses one across a resync.
    }

    public mutating func apply(_ event: HarnessEvent) {
        // The harness blocks only at the moment it asks. Observing anything
        // else after a request means it moved on — the prompt was answered,
        // here or at the computer — so the buttons stop being offered.
        if pendingRequest != nil, case .awaitingInput = event.payload {} else {
            pendingRequest = nil
        }

        switch event.payload {
        case .userMessage(let text):
            // A message this device sent arrives back through the ledger, so
            // the transcript shows what the session actually received rather
            // than a local echo that might not have landed.
            endStreaming()
            append(at: event.at, kind: .user(text))

        case .assistantDelta(let text):
            if let index = streamingIndex,
               case .assistant(let existing) = entries[index].kind {
                entries[index].kind = .assistant(existing + text)
            } else {
                append(at: event.at, kind: .assistant(text))
                streamingIndex = entries.count - 1
            }

        case .assistantMessage(let text):
            // The complete message supersedes whatever the deltas built: it is
            // the harness's own final rendering of the same turn.
            if let index = streamingIndex {
                entries[index].kind = .assistant(text)
                streamingIndex = nil
            } else {
                append(at: event.at, kind: .assistant(text))
            }

        case .toolStarted(let tool, let input):
            endStreaming()
            append(
                at: event.at,
                kind: .tool(name: tool, detail: input.summary, finished: false)
            )

        case .toolFinished(let tool, let output):
            // Complete the matching started row when there is one, so a tool
            // call is one line in the transcript instead of two.
            if let index = entries.lastIndex(where: { entry in
                if case .tool(let name, _, let finished) = entry.kind {
                    return name == tool && !finished
                }
                return false
            }), case .tool(_, let detail, _) = entries[index].kind {
                let summary = output.summary
                entries[index].kind = .tool(
                    name: tool,
                    detail: summary.isEmpty ? detail : summary,
                    finished: true
                )
            } else {
                append(
                    at: event.at,
                    kind: .tool(name: tool, detail: output.summary, finished: true)
                )
            }

        case .awaitingInput(let requestId, let kind, let prompt, let choices):
            endStreaming()
            let request = TranscriptEntry.Request(
                requestId: requestId,
                kind: kind,
                prompt: prompt,
                choices: choices ?? []
            )
            pendingRequest = request
            append(at: event.at, kind: .request(request))

        case .status(let status):
            endStreaming()
            self.status = status
            append(at: event.at, kind: .status(status))

        case .unknown(let type):
            endStreaming()
            append(at: event.at, kind: .unrecognized(type))
        }
    }

    /// Records that a choice was submitted for `requestId`.
    ///
    /// The harness reports the real outcome through a later event; this only
    /// stops the UI offering the same buttons while that is in flight.
    public mutating func markAnswered(requestId: String, choice: String) {
        for index in entries.indices {
            if case .request(var request) = entries[index].kind,
               request.requestId == requestId {
                request.answered = choice
                entries[index].kind = .request(request)
            }
        }
        if pendingRequest?.requestId == requestId {
            pendingRequest = nil
        }
    }

    private mutating func append(at: String, kind: TranscriptEntry.Kind) {
        entries.append(TranscriptEntry(id: nextId, at: at, kind: kind))
        nextId += 1
    }

    private mutating func endStreaming() {
        streamingIndex = nil
    }
}
