import Foundation

public protocol ConversationStoreStorage {
    /// The cache as last recorded: the base snapshot with any later journal
    /// entries already folded in.
    func load(sessionID: String) throws -> ConversationStoreCache?
    /// Replaces the whole cache and discards the journal. The store calls this
    /// only when a snapshot replaces the transcript or the journal is compacted.
    func save(_ cache: ConversationStoreCache, sessionID: String) throws
    /// Records one incremental change after the last `save` and returns the
    /// bytes the journal now holds, which the store uses to decide when to
    /// compact. Storages without a journal return 0.
    func append(_ entry: ConversationJournalEntry, sessionID: String) throws -> Int
}

extension ConversationStoreStorage {
    /// Fallback for storages without a journal. Correct, but a full rewrite:
    /// `FileConversationStoreStorage` implements a real append.
    public func append(_ entry: ConversationJournalEntry, sessionID: String) throws -> Int {
        let cache = try load(sessionID: sessionID) ?? ConversationStoreCache()
        try save(cache.applying([entry]), sessionID: sessionID)
        return 0
    }
}

public struct ConversationStoreCache: Codable, Equatable, Sendable {
    public var generation: String?
    public var revision: UInt64
    public var operationEpoch: String?
    public var items: [ConversationItem]
    public var state: ConversationState?
    public var hasMoreBefore: Bool
    public var operations: [ConversationOperation]
    /// The last journal entry this cache already reflects. Entries at or below
    /// it are stale leftovers of an interrupted compaction and are ignored.
    public var journalSequence: UInt64?

    public init(
        generation: String? = nil,
        revision: UInt64 = 0,
        operationEpoch: String? = nil,
        items: [ConversationItem] = [],
        state: ConversationState? = nil,
        hasMoreBefore: Bool = false,
        operations: [ConversationOperation] = [],
        journalSequence: UInt64? = nil
    ) {
        self.generation = generation
        self.revision = revision
        self.operationEpoch = operationEpoch
        self.items = items
        self.state = state
        self.hasMoreBefore = hasMoreBefore
        self.operations = operations
        self.journalSequence = journalSequence
    }

    /// Folds journal entries over this cache. Replay stops at the first gap in
    /// the sequence: every entry carries the revision its items belong to, so a
    /// contiguous prefix is always a consistent (if older) resume position.
    public func applying(_ entries: [ConversationJournalEntry]) -> ConversationStoreCache {
        var expected = (journalSequence ?? 0) &+ 1
        var result = self
        var byID: [String: ConversationItem]?
        for entry in entries where entry.sequence >= expected {
            guard entry.sequence == expected else { break }
            if byID == nil { byID = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { _, new in new }) }
            entry.removedIDs.forEach { byID?[$0] = nil }
            entry.upserts.forEach { byID?[$0.id] = $0 }
            result.generation = entry.generation
            result.revision = entry.revision
            result.operationEpoch = entry.operationEpoch
            result.state = entry.state
            result.hasMoreBefore = entry.hasMoreBefore
            result.operations = entry.operations
            result.journalSequence = entry.sequence
            expected = entry.sequence &+ 1
        }
        if let byID { result.items = byID.values.sorted { $0.ordinal < $1.ordinal } }
        return result
    }
}

/// One incremental cache change: the items that changed or left since the
/// previous entry, plus the small, whole conversation metadata at that point.
public struct ConversationJournalEntry: Codable, Equatable, Sendable {
    public var sequence: UInt64
    public var generation: String?
    public var revision: UInt64
    public var operationEpoch: String?
    public var state: ConversationState?
    public var hasMoreBefore: Bool
    public var operations: [ConversationOperation]
    public var upserts: [ConversationItem]
    public var removedIDs: [String]

    public init(
        sequence: UInt64,
        generation: String?,
        revision: UInt64,
        operationEpoch: String?,
        state: ConversationState?,
        hasMoreBefore: Bool,
        operations: [ConversationOperation],
        upserts: [ConversationItem],
        removedIDs: [String]
    ) {
        self.sequence = sequence
        self.generation = generation
        self.revision = revision
        self.operationEpoch = operationEpoch
        self.state = state
        self.hasMoreBefore = hasMoreBefore
        self.operations = operations
        self.upserts = upserts
        self.removedIDs = removedIDs
    }
}
