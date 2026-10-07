import Foundation

public enum ConversationOperationStatus: String, Codable, Equatable, Sendable {
    case sending
    case queued
    case refused
    case ambiguous
    case manualReview = "manual_review"
}

/// A locally initiated operation remains separate from Hub-owned items.  This
/// lets a refusal retain the person's text for retry without inventing a
/// conversation mutation, and makes ambiguous delivery unmistakable.
public struct ConversationOperation: Codable, Equatable, Identifiable, Sendable {
    public let id: String
    public let text: String
    public let operationEpoch: String
    public let createdAt: Date
    public var status: ConversationOperationStatus
    public var reason: String?
    public var itemId: String?

    public init(
        id: String,
        text: String,
        operationEpoch: String,
        createdAt: Date = .now,
        status: ConversationOperationStatus = .sending,
        reason: String? = nil,
        itemId: String? = nil
    ) {
        self.id = id
        self.text = text
        self.operationEpoch = operationEpoch
        self.createdAt = createdAt
        self.status = status
        self.reason = reason
        self.itemId = itemId
    }

    /// The transcript row shown for this operation until the host's own item
    /// replaces it.
    public var optimisticItemID: String { "operation:\(id)" }
}

public enum ConversationResolveStatus: Equatable, Sendable {
    /// Sent; the host has not answered yet.
    case sending
    /// The host applied the answer. The request settles when the transcript
    /// says so.
    case accepted
    /// The host declined to apply the answer, for the reason it gave.
    case refused
    /// It is unknown whether the host applied the answer.
    case ambiguous
    /// The answer never left the phone.
    case notSent
}

/// One explicit answer to one request. Like a send, each answer is its own
/// operation: answering again after a refusal is a new attempt, never a
/// replay of the old one.
public struct ConversationResolveAttempt: Equatable, Identifiable, Sendable {
    /// The operation id sent with the answer.
    public let id: String
    public let requestId: String
    public let choice: String
    public var status: ConversationResolveStatus
    public var reason: String?

    public init(
        id: String,
        requestId: String,
        choice: String,
        status: ConversationResolveStatus = .sending,
        reason: String? = nil
    ) {
        self.id = id
        self.requestId = requestId
        self.choice = choice
        self.status = status
        self.reason = reason
    }

    /// An answer on its way or already applied is not offered again.
    public var isInFlight: Bool { status == .sending || status == .accepted }
}

