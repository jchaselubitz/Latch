import Foundation

// What the bottom of the chat screen offers: the pending request's choices,
// or the composer and why it cannot send, plus the sends whose outcome needs
// the person. Like the transcript projection, these are pure values derived
// from store facts, so every wording and decision is tested without a view.

/// What happened to the answer this phone gave to a request.
public struct ConversationRequestAnswerPresentation: Equatable, Sendable {
    public enum Outcome: Equatable, Sendable {
        case submitting
        case submitted
        /// The host declined to apply it. An expected outcome, not an error.
        case refused(String)
        /// It may or may not have been applied.
        case uncertain(String)
        /// It never left the phone.
        case notSent(String)
    }

    public let choice: String
    public let outcome: Outcome

    public init(choice: String, outcome: Outcome) {
        self.choice = choice
        self.outcome = outcome
    }

    public init(attempt: ConversationResolveAttempt) {
        let reason = attempt.reason.map(ConversationSentence.make) ?? ""
        switch attempt.status {
        case .sending: self.init(choice: attempt.choice, outcome: .submitting)
        case .accepted: self.init(choice: attempt.choice, outcome: .submitted)
        case .refused: self.init(choice: attempt.choice, outcome: .refused(reason))
        case .ambiguous: self.init(choice: attempt.choice, outcome: .uncertain(reason))
        case .notSent: self.init(choice: attempt.choice, outcome: .notSent(reason))
        }
    }

    /// Whether the choices are offered again. An answer on its way or already
    /// applied is offered once; any other outcome takes a new explicit choice.
    public var allowsAnotherAnswer: Bool {
        switch outcome {
        case .submitting, .submitted: false
        case .refused, .uncertain, .notSent: true
        }
    }

    /// Whether a settled request row still has something to say about this
    /// answer. One that went through is already told by the row's outcome.
    public var explainsSettledRequest: Bool {
        switch outcome {
        case .submitting, .submitted: false
        case .refused, .uncertain, .notSent: true
        }
    }

    public var title: String {
        switch outcome {
        case .submitting: "Sending “\(choice)”…"
        case .submitted: "Sent “\(choice)”"
        case .refused: "“\(choice)” was not applied"
        case .uncertain: "“\(choice)” may have been applied"
        case .notSent: "“\(choice)” was not sent"
        }
    }

    public var detail: String? {
        switch outcome {
        case .submitting: nil
        case .submitted: "Waiting for the agent to take the answer."
        case .refused(let reason): Self.join(reason, "Choose again if the request is still open.")
        case .uncertain(let reason): Self.join(reason, "Check the transcript before answering again.")
        case .notSent(let reason): Self.join(reason, "Choose again once connected.")
        }
    }

    private static func join(_ reason: String, _ advice: String) -> String {
        reason.isEmpty ? advice : reason + " " + advice
    }
}

/// Why the composer cannot send right now, as a state rather than an error.
public struct ConversationSendNotice: Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        /// The agent is busy or waiting on the person: nothing is wrong.
        case agentState
        /// The phone is not connected to the Mac right now.
        case connection
        /// The host reports sending is unavailable for another reason.
        case unavailable
    }

    public let kind: Kind
    public let title: String
    public let detail: String?

    public init(kind: Kind, title: String, detail: String?) {
        self.kind = kind
        self.title = title
        self.detail = detail
    }
}

/// The ordinary composer. The draft stays editable in every state; only
/// sending follows the host.
public struct ConversationComposerPresentation: Equatable, Sendable {
    public let canSend: Bool
    public let notice: ConversationSendNotice?

    public init(canSend: Bool, notice: ConversationSendNotice?) {
        self.canSend = canSend
        self.notice = notice
    }

    public static func derive(
        viewState: ConversationViewState,
        state: ConversationState?,
        canSend: Bool,
        sendReason: String?,
        connectionError: String?
    ) -> ConversationComposerPresentation {
        let hostReason = sendReason.map(ConversationSentence.make)
        switch viewState {
        case .disconnected:
            // The last pushed state may still say sending is enabled; it is
            // stale until the socket is back.
            return blocked(.connection, "Offline", connectionError.map(ConversationSentence.make)
                ?? "Reconnecting to the Mac. Your draft is kept.")
        case .loading:
            return blocked(.connection, "Connecting", hostReason)
        case .failed:
            return blocked(.unavailable, "Sending unavailable", connectionError.map(ConversationSentence.make) ?? hostReason)
        case .empty, .ready, .working, .awaitingInput, .interrupted:
            break
        }
        if canSend { return ConversationComposerPresentation(canSend: true, notice: nil) }
        // The host refuses a send while a request is pending or a tool runs.
        // Both are the agent doing its job, and are said that way.
        if state?.pendingRequest != nil || viewState == .awaitingInput {
            return blocked(.agentState, "Waiting for your answer", "Answer the pending request to continue.")
        }
        if viewState == .working {
            return blocked(.agentState, "The agent is working", "You can send when the host reports it is ready for input.")
        }
        if viewState == .interrupted {
            return blocked(.agentState, "The agent has exited", hostReason)
        }
        return blocked(.unavailable, "Sending unavailable", hostReason)
    }

    private static func blocked(_ kind: ConversationSendNotice.Kind, _ title: String, _ detail: String?) -> Self {
        ConversationComposerPresentation(canSend: false, notice: ConversationSendNotice(kind: kind, title: title, detail: detail))
    }
}

/// A send whose outcome needs the person. Refused and uncertain sends look
/// different and offer different ways out; every way out that sends is a new
/// operation, and none happens without an explicit tap.
public struct ConversationOperationPresentation: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        /// The host did not take it. Nothing was delivered.
        case refused
        /// It may have been delivered.
        case uncertain
        /// No outcome can be established; the person decides.
        case needsReview
    }

    public enum Action: Equatable, Sendable {
        /// Put the text back in the composer.
        case edit
        /// Send the same text as a new operation.
        case sendAgain
        /// Forget this record.
        case dismiss
    }

    public let id: String
    public let text: String
    public let kind: Kind
    public let title: String
    public let detail: String
    public let actions: [Action]

    /// Nil while the operation is still sending: the transcript row already
    /// says so, and there is nothing to decide yet.
    public init?(operation: ConversationOperation) {
        id = operation.id
        text = operation.text
        let reason = operation.reason.map(ConversationSentence.make)
        switch operation.status {
        case .sending:
            return nil
        case .refused:
            kind = .refused
            title = "Not sent"
            detail = reason ?? "The host did not accept this message."
            actions = [.edit, .sendAgain, .dismiss]
        case .ambiguous:
            kind = .uncertain
            title = "May have been delivered"
            detail = (reason.map { $0 + " " } ?? "") + "Check the transcript before sending it again."
            actions = [.sendAgain, .dismiss]
        case .manualReview:
            kind = .needsReview
            title = "Not confirmed"
            detail = reason ?? "Review this message before sending it again."
            actions = [.edit, .sendAgain, .dismiss]
        }
    }

    public static func rows(for operations: [ConversationOperation]) -> [ConversationOperationPresentation] {
        operations.compactMap(ConversationOperationPresentation.init(operation:))
    }

    public func label(for action: Action) -> String {
        switch action {
        case .edit: "Edit"
        // Past an uncertain outcome, the label says a second copy may arrive.
        case .sendAgain: kind == .refused ? "Send again" : "Send as new message"
        case .dismiss: "Dismiss"
        }
    }
}

/// Host reasons arrive as lower-case fragments ("agent is working"); shown
/// on their own they read as sentences.
enum ConversationSentence {
    static func make(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = trimmed.first else { return trimmed }
        let capitalized = first.uppercased() + trimmed.dropFirst()
        return ".!?…".contains(capitalized.last!) ? capitalized : capitalized + "."
    }
}
