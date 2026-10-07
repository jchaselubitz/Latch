import Foundation
import Observation

/// Observable conversation state and initialization. Behavior is grouped in
/// the ConversationStore extensions; shared implementation state is internal
/// so those files can mutate it without exposing setters to kit clients.
@MainActor
@Observable
public final class ConversationStore {
    // MARK: - Observable state

    /// Rows currently offered to the view. The complete local history is in
    /// `retainedItems`; moving this window never evicts that history.
    public internal(set) var items: [ConversationItem]

    public internal(set) var state: ConversationState?

    public internal(set) var generation: String?

    public internal(set) var revision: UInt64

    public internal(set) var operationEpoch: String?

    public internal(set) var hasMoreBefore: Bool

    public internal(set) var operations: [ConversationOperation]

    public internal(set) var socketState: ConversationSocketState = .idle

    public internal(set) var connectionError: String?

    public internal(set) var prependAnchor: String?

    /// Cumulative for this store's lifetime; content-free by construction.
    public internal(set) var decodeDiagnostics = ConversationDecodeDiagnostics()

    /// The message being written. It lives with the store, which AppModel
    /// keeps for the whole link, so it survives leaving the chat and every
    /// reconnect. It is deliberately not written to disk.
    public var draft = ""

    /// The latest answer this phone gave to each request, oldest first. Kept
    /// in memory only: the Hub's state remains the authority on what is
    /// pending, and these only explain what happened to an answer.
    public internal(set) var resolveAttempts: [ConversationResolveAttempt] = []

    public internal(set) var cancelAttempt: ConversationResolveAttempt?

    /// Files waiting to go out with the next message. In memory only, like
    /// the draft, and kept across a failed send so nothing is lost.
    public internal(set) var attachments: [ConversationAttachment] = []

    public internal(set) var attachmentPhase: ConversationAttachmentPhase = .idle

    /// The upload-then-send in flight, for tests to await.
    @ObservationIgnored var attachmentSendTask: Task<Void, Never>?

    @ObservationIgnored let attachmentUploader: (any ConversationAttachmentUploading)?

    // MARK: - Shared implementation state

    public let sessionID: String

    let storage: any ConversationStoreStorage

    let maximumItems: Int

    let maximumBytes: Int

    let maximumRenderedItems = 300

    let persistInterval: Duration

    var gateway: LatchGateway

    var retentionSeconds: TimeInterval

    var socket: ConversationSocket?

    var transcript = RetainedTranscript()

    var renderedEnd: Int

    var serverHasMoreBefore: Bool

    var publishTask: Task<Void, Never>?

    var isStarted = false

    var resyncRequestedAtRevision: UInt64?

    // Persistence is a journal of what changed since the last entry, written
    // at most once per `persistInterval` while a turn streams, and immediately
    // for operation and history changes. See `flushPersistence`.
    @ObservationIgnored var persistTask: Task<Void, Never>?

    @ObservationIgnored var journalSequence: UInt64

    @ObservationIgnored var dirtyItemIDs: Set<String> = []

    @ObservationIgnored var removedItemIDs: Set<String> = []

    @ObservationIgnored var hasUnpersistedChanges = false

    @ObservationIgnored var needsCompaction = false

    // MARK: - Initialization

    public init(
        sessionID: String,
        gateway: LatchGateway,
        operationRetentionSeconds: Int,
        storage: any ConversationStoreStorage = FileConversationStoreStorage(),
        maximumItems: Int = 10_000,
        maximumBytes: Int = 32 * 1024 * 1024,
        persistInterval: Duration = .milliseconds(500),
        attachmentUploader: (any ConversationAttachmentUploading)? = nil
    ) {
        self.sessionID = sessionID
        self.gateway = gateway
        self.attachmentUploader = attachmentUploader
        retentionSeconds = TimeInterval(max(0, operationRetentionSeconds))
        self.storage = storage
        self.maximumItems = maximumItems
        self.maximumBytes = maximumBytes
        self.persistInterval = persistInterval
        let cached = (try? storage.load(sessionID: sessionID)) ?? ConversationStoreCache()
        journalSequence = cached.journalSequence ?? 0
        generation = cached.generation
        revision = cached.revision
        operationEpoch = cached.operationEpoch
        var restored = RetainedTranscript()
        restored.replaceAll(cached.items)
        let evicted = restored.evict(maximumItems: maximumItems, maximumBytes: maximumBytes)
        transcript = restored
        renderedEnd = restored.count
        items = Array(restored.items.suffix(maximumRenderedItems))
        state = cached.state
        serverHasMoreBefore = cached.hasMoreBefore
        hasMoreBefore = cached.hasMoreBefore && restored.count < maximumItems
        operations = cached.operations
        if !evicted.isEmpty { noteChanges(upserted: [], removed: evicted) }
        publishRenderedItems()
    }
}
