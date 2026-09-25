import Foundation
import Observation

public enum ConversationOperationStatus: String, Codable, Equatable, Sendable {
    case sending
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

/// Disk-backed, per-session cache: a base snapshot (`<session>.json`) plus an
/// append-only journal of newline-delimited entries (`<session>.journal`).
/// Existing v1 derived event caches are not consulted or migrated: v2
/// snapshots are a complete replacement boundary.
///
/// On iOS, every cache path uses `completeUntilFirstUserAuthentication` and
/// is excluded from device backup. This is deliberately not `.complete`: the
/// store can need to append a recovery or reconnect update after the device
/// locks, and `.completeUnlessOpen` would not cover a journal opened after
/// that lock. The selected class remains protected at rest while allowing the
/// background resume path to persist after the user's first unlock.
public final class FileConversationStoreStorage: ConversationStoreStorage, @unchecked Sendable {
    private let directory: URL
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    public init(directory: URL? = nil) {
        self.directory = directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
                .appendingPathComponent("Latch", isDirectory: true)
                .appendingPathComponent("conversations", isDirectory: true)
    }

    public func load(sessionID: String) throws -> ConversationStoreCache? {
        try prepareDirectory()
        let url = fileURL(sessionID)
        try protectExistingCacheFiles(sessionID)
        let base = FileManager.default.fileExists(atPath: url.path)
            ? try decoder.decode(ConversationStoreCache.self, from: Data(contentsOf: url))
            : nil
        let entries = journalEntries(sessionID)
        guard base != nil || !entries.isEmpty else { return nil }
        return (base ?? ConversationStoreCache()).applying(entries)
    }

    public func save(_ cache: ConversationStoreCache, sessionID: String) throws {
        try prepareDirectory()
        let url = fileURL(sessionID)
        try encoder.encode(cache).write(to: url, options: .atomic)
        try protect(url)
        // The new base records the journal sequence it covers, so an
        // interruption before this removal leaves only ignorable entries.
        try? FileManager.default.removeItem(at: journalURL(sessionID))
    }

    public func append(_ entry: ConversationJournalEntry, sessionID: String) throws -> Int {
        try prepareDirectory()
        let url = journalURL(sessionID)
        if !FileManager.default.fileExists(atPath: url.path) {
            guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
                throw CocoaError(.fileWriteUnknown)
            }
        }
        // The file may predate protection (or have been recreated after a
        // snapshot), so apply it on every append rather than relying on the
        // directory's attributes to propagate.
        try protect(url)
        // Compact JSON escapes every control character, so a newline can only
        // be the record separator.
        var line = try encoder.encode(entry)
        line.append(0x0A)
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }
        let end = try handle.seekToEnd()
        try handle.write(contentsOf: line)
        return Int(end) + line.count
    }

    private func journalEntries(_ sessionID: String) -> [ConversationJournalEntry] {
        guard let data = try? Data(contentsOf: journalURL(sessionID)) else { return [] }
        var entries: [ConversationJournalEntry] = []
        for line in data.split(separator: 0x0A) {
            // A record torn by an interrupted write ends the usable prefix.
            guard let entry = try? decoder.decode(ConversationJournalEntry.self, from: Data(line)) else { break }
            entries.append(entry)
        }
        return entries
    }

    private func fileURL(_ sessionID: String) -> URL {
        directory.appendingPathComponent("\(safeName(sessionID)).json")
    }

    private func journalURL(_ sessionID: String) -> URL {
        directory.appendingPathComponent("\(safeName(sessionID)).journal")
    }

    private func safeName(_ sessionID: String) -> String {
        sessionID.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) ? String($0) : "_" }.joined()
    }

    private func prepareDirectory() throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try protect(directory)
    }

    private func protectExistingCacheFiles(_ sessionID: String) throws {
        for url in [fileURL(sessionID), journalURL(sessionID)] where FileManager.default.fileExists(atPath: url.path) {
            try protect(url)
        }
    }

    private func protect(_ url: URL) throws {
        #if os(iOS)
        try FileManager.default.setAttributes(
            [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
            ofItemAtPath: url.path
        )
        var mutableURL = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try mutableURL.setResourceValues(values)
        #endif
    }
}

@MainActor
@Observable
public final class ConversationStore {
    /// Rows currently offered to the view. The complete local history is in
    /// `retainedItems`; moving this window never evicts that history.
    public private(set) var items: [ConversationItem]
    public var retainedItems: [ConversationItem] { transcript.items }
    public var hasEarlierRendered: Bool { renderedEnd > items.count }
    public var hasNewerRendered: Bool { renderedEnd < transcript.count }
    public var isHistoryLimitReached: Bool { serverHasMoreBefore && !hasMoreBefore }
    public private(set) var state: ConversationState?
    public private(set) var generation: String?
    public private(set) var revision: UInt64
    public private(set) var operationEpoch: String?
    public private(set) var hasMoreBefore: Bool
    public private(set) var operations: [ConversationOperation]
    public private(set) var socketState: ConversationSocketState = .idle
    public private(set) var connectionError: String?
    public private(set) var prependAnchor: String?
    /// Cumulative for this store's lifetime; content-free by construction.
    public private(set) var decodeDiagnostics = ConversationDecodeDiagnostics()
    /// The message being written. It lives with the store, which AppModel
    /// keeps for the whole link, so it survives leaving the chat and every
    /// reconnect. It is deliberately not written to disk.
    public var draft = ""
    /// The latest answer this phone gave to each request, oldest first. Kept
    /// in memory only: the Hub's state remains the authority on what is
    /// pending, and these only explain what happened to an answer.
    public private(set) var resolveAttempts: [ConversationResolveAttempt] = []
    private static let maximumResolveAttempts = 20
    /// Files waiting to go out with the next message. In memory only, like
    /// the draft, and kept across a failed send so nothing is lost.
    public private(set) var attachments: [ConversationAttachment] = []
    public private(set) var attachmentPhase: ConversationAttachmentPhase = .idle
    /// The upload-then-send in flight, for tests to await.
    @ObservationIgnored var attachmentSendTask: Task<Void, Never>?
    @ObservationIgnored private let attachmentUploader: (any ConversationAttachmentUploading)?

    public let sessionID: String
    private let storage: any ConversationStoreStorage
    private let maximumItems: Int
    private let maximumBytes: Int
    private let maximumRenderedItems = 300
    private let persistInterval: Duration
    private var gateway: LatchGateway
    private var retentionSeconds: TimeInterval
    private var socket: ConversationSocket?
    private var transcript = RetainedTranscript()
    private var renderedEnd: Int
    private var serverHasMoreBefore: Bool
    private var publishTask: Task<Void, Never>?
    private var isStarted = false
    private var resyncRequestedAtRevision: UInt64?

    // Persistence is a journal of what changed since the last entry, written
    // at most once per `persistInterval` while a turn streams, and immediately
    // for operation and history changes. See `flushPersistence`.
    @ObservationIgnored private var persistTask: Task<Void, Never>?
    @ObservationIgnored private var journalSequence: UInt64
    @ObservationIgnored private var dirtyItemIDs: Set<String> = []
    @ObservationIgnored private var removedItemIDs: Set<String> = []
    @ObservationIgnored private var hasUnpersistedChanges = false
    @ObservationIgnored private var needsCompaction = false
    /// Below this the journal is never compacted: rewriting a small cache to
    /// save replaying a few entries is not worth the write.
    private static let minimumCompactionBytes = 256 * 1024

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

    public var canSend: Bool { state?.sendMessage.enabled == true && operationEpoch != nil }
    public var sendReason: String? { state?.sendMessage.reason ?? (operationEpoch == nil ? "Waiting for conversation state" : nil) }
    public var canResolve: Bool { state?.resolveRequest.enabled == true && operationEpoch != nil }
    public var resolveReason: String? { state?.resolveRequest.reason }
    public var pendingRequest: ConversationItem? {
        guard let requestID = state?.pendingRequest else { return nil }
        return transcript.items.last { item in
            if case .request(let id, _, _, _, _) = item.kind { return id == requestID }
            return false
        }
    }

    /// Items measured for the byte budget over this store's lifetime. Each
    /// item is measured once when it enters or changes, never per publish.
    var measuredItemCount: Int { transcript.measuredItemCount }

    /// Restored content is already published by init; this only begins network
    /// observation. Keeping the store in AppModel means leaving a chat does not
    /// restart from the beginning when the view returns.
    public func start() {
        guard !isStarted else { return }
        isStarted = true
        let socket = makeSocket()
        self.socket = socket
        Task { await socket.start(position: resumePosition) }
    }

    public func stop() {
        // Stopping precedes suspension, so nothing waits on the throttle.
        flushPersistence()
        isStarted = false
        if let socket { Task { await socket.stop() } }
        socket = nil
    }

    public func reconnect(using gateway: LatchGateway, operationRetentionSeconds: Int) {
        self.gateway = gateway
        retentionSeconds = TimeInterval(max(0, operationRetentionSeconds))
        let shouldRestart = isStarted
        stop()
        if shouldRestart { start() }
    }

    public func send(text: String) {
        guard attachments.isEmpty else {
            sendWithAttachments(text: text)
            return
        }
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, canSend, let operationEpoch else { return }
        enqueue(text: text, operationEpoch: operationEpoch)
    }

    private func enqueue(text: String, operationEpoch: String) {
        let operation = ConversationOperation(id: UUID().uuidString, text: text, operationEpoch: operationEpoch)
        operations.append(operation)
        appendOptimisticItem(for: operation)
        persistNow()
        send(operation: operation)
    }

    public var isUploadingAttachments: Bool { attachmentPhase == .uploading }

    /// Adds a file to the next message. A file larger than the Mac accepts is
    /// refused here, with the reason in `attachmentPhase`, rather than
    /// uploaded only to be turned away.
    @discardableResult
    public func addAttachment(_ attachment: ConversationAttachment, maximumBytes: Int?) -> Bool {
        guard !isUploadingAttachments else { return false }
        if let maximumBytes, attachment.byteCount > maximumBytes {
            let limit = ByteCountFormatter.string(fromByteCount: Int64(maximumBytes), countStyle: .file)
            attachmentPhase = .failed("\(attachment.name) is larger than the \(limit) your Mac accepts.")
            return false
        }
        attachments.append(attachment)
        attachmentPhase = .idle
        return true
    }

    /// Takes a file back out of the next message. Not while it is uploading.
    public func removeAttachment(_ id: UUID) {
        guard !isUploadingAttachments else { return }
        attachments.removeAll { $0.id == id }
        if case .failed = attachmentPhase { attachmentPhase = .idle }
    }

    /// Uploads every file the Mac does not already have, then sends the text
    /// with their paths appended as one ordinary message.
    ///
    /// The order matters: the agent must never read a path before the file is
    /// there. So nothing is sent until every upload has succeeded. Any failure
    /// sends nothing, returns the text to the draft, and keeps the files —
    /// with the receipts of those that did arrive, so a retry uploads only
    /// what is missing.
    private func sendWithAttachments(text: String) {
        guard !isUploadingAttachments, canSend else {
            restoreDraft(text)
            return
        }
        attachmentPhase = .uploading
        let uploader: any ConversationAttachmentUploading = attachmentUploader ?? gateway
        let sessionID = sessionID
        attachmentSendTask = Task { [weak self] in
            guard let self else { return }
            // Adding and removing are refused while uploading, so these
            // indices stay valid across each await.
            for index in attachments.indices where attachments[index].receipt == nil {
                let attachment = attachments[index]
                do {
                    attachments[index].receipt = try await uploader.uploadAttachment(
                        sessionID: sessionID,
                        name: attachment.name,
                        data: attachment.data
                    )
                } catch {
                    failAttachmentSend(text: text, reason: Self.uploadFailureReason(error))
                    return
                }
            }
            // The agent may have become busy while the files uploaded. They
            // stay on the Mac and in the composer; the next send reuses them.
            guard canSend, let operationEpoch else {
                failAttachmentSend(
                    text: text,
                    reason: sendReason ?? "The agent cannot take a message right now."
                )
                return
            }
            let message = ConversationAttachmentMessage.compose(
                text: text,
                paths: attachments.compactMap { $0.receipt?.path }
            )
            attachments = []
            attachmentPhase = .idle
            enqueue(text: message, operationEpoch: operationEpoch)
        }
    }

    private func failAttachmentSend(text: String, reason: String) {
        attachmentPhase = .failed(reason)
        restoreDraft(text)
    }

    /// Puts unsent text back in front of anything typed since.
    private func restoreDraft(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? text
            : text + "\n\n" + draft
    }

    private static func uploadFailureReason(_ error: Error) -> String {
        if let error = error as? LatchError {
            if case .refused(let reason) = error { return reason }
            return "The attachment did not reach your Mac. \(error.message)"
        }
        return "The attachment did not reach your Mac. \(error.localizedDescription)"
    }

    /// An explicit retry is always a new operation. In particular, ambiguous
    /// operations may already have reached the kernel and must never be replayed.
    /// The settled record it replaces is dismissed, so one explicit choice
    /// cannot be offered, and taken, twice.
    public func retry(_ operationID: String) {
        guard let operation = operations.first(where: { $0.id == operationID }),
              operation.status != .sending,
              canSend
        else { return }
        dismissOperation(operationID)
        send(text: operation.text)
    }

    /// Forgets a settled operation the person has reviewed, along with its
    /// transcript row. One still sending stays: its outcome is still coming.
    public func dismissOperation(_ operationID: String) {
        guard let index = operations.firstIndex(where: { $0.id == operationID }),
              operations[index].status != .sending
        else { return }
        let operation = operations.remove(at: index)
        removeItems([operation.optimisticItemID])
        publishImmediately()
        persistNow()
    }

    /// Returns a settled operation's exact text to the composer for editing,
    /// after anything already typed, and dismisses the operation. Nothing is
    /// sent until the person sends it.
    public func editOperation(_ operationID: String) {
        guard let operation = operations.first(where: { $0.id == operationID }),
              operation.status != .sending
        else { return }
        draft = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? operation.text
            : draft + "\n\n" + operation.text
        dismissOperation(operationID)
    }

    public func resolveAttempt(for requestID: String) -> ConversationResolveAttempt? {
        resolveAttempts.last { $0.requestId == requestID }
    }

    /// Answers the exact request the host names as pending. Each answer is a
    /// new operation. A request whose answer is in flight or already accepted
    /// is not answered again; after a refusal or an uncertain outcome only
    /// another explicit choice answers it.
    public func resolve(requestID: String, choice: String) {
        guard canResolve,
              let operationEpoch,
              state?.pendingRequest == requestID,
              resolveAttempt(for: requestID)?.isInFlight != true
        else { return }
        let attempt = ConversationResolveAttempt(id: UUID().uuidString, requestId: requestID, choice: choice)
        resolveAttempts.removeAll { $0.requestId == requestID }
        resolveAttempts.append(attempt)
        if resolveAttempts.count > Self.maximumResolveAttempts {
            resolveAttempts.removeFirst(resolveAttempts.count - Self.maximumResolveAttempts)
        }
        let socket = socket
        Task {
            guard let socket else {
                sendFailed(attempt.id, status: .notSent, reason: "The conversation is not connected.")
                return
            }
            do {
                try await socket.send(.resolveRequest(
                    operationEpoch: operationEpoch,
                    operationId: attempt.id,
                    requestId: requestID,
                    choice: choice
                ))
            } catch let error as ConversationSocketError where error == .notConnected {
                sendFailed(attempt.id, status: .notSent, reason: "The conversation is not connected.")
            } catch {
                // A failed write may still have left the phone.
                sendFailed(attempt.id, status: .ambiguous, reason: error.localizedDescription)
            }
        }
    }

    /// A local send failure never overrides an outcome the host already gave.
    private func sendFailed(_ operationID: String, status: ConversationResolveStatus, reason: String) {
        guard resolveAttempts.first(where: { $0.id == operationID })?.status == .sending else { return }
        updateResolveAttempt(operationID, status: status, reason: reason)
    }

    private func updateResolveAttempt(_ operationID: String, status: ConversationResolveStatus, reason: String?) {
        guard let index = resolveAttempts.firstIndex(where: { $0.id == operationID }) else { return }
        resolveAttempts[index].status = status
        resolveAttempts[index].reason = reason
    }

    /// An accepted answer to a request the host no longer names as pending
    /// has done its job; the transcript row now says how it settled. Refused
    /// and uncertain answers stay so the settled row can still explain them.
    private func pruneSettledResolveAttempts() {
        let pending = state?.pendingRequest
        resolveAttempts.removeAll { $0.status == .accepted && $0.requestId != pending }
    }

    public func loadOlder(limit: Int = 100) {
        if hasEarlierRendered {
            prependAnchor = items.first?.id
            renderedEnd = max(maximumRenderedItems, renderedEnd - min(100, max(1, limit)))
            publishRenderedItems()
            return
        }
        // Optimistic rows sort last, so the first retained row is the oldest.
        guard hasMoreBefore, let oldest = transcript.items.first?.ordinal, oldest != UInt64.max else { return }
        let requestID = UUID().uuidString
        Task {
            do {
                try await socket?.send(.historyRequest(requestId: requestID, beforeOrdinal: oldest, limit: min(100, max(1, limit))))
            } catch {
                connectionError = error.localizedDescription
            }
        }
    }

    public func showNewer() {
        guard hasNewerRendered else { return }
        renderedEnd = min(transcript.count, renderedEnd + 100)
        publishRenderedItems()
    }

    private var resumePosition: ConversationResumePosition {
        ConversationResumePosition(generation: generation, afterRevision: generation == nil ? nil : revision, operationEpoch: operationEpoch)
    }

    private func makeSocket() -> ConversationSocket {
        ConversationSocket(
            makeConnection: { [gateway, sessionID] position in
                try await gateway.openConversation(sessionID: sessionID, position: position)
            },
            eventHandler: { [weak self] event in
                await self?.receive(event)
            }
        )
    }

    func receive(_ event: ConversationSocketEvent) {
        switch event {
        case .state(let socketState):
            self.socketState = socketState
            if socketState == .open {
                connectionError = nil
                replayRetainedOperations()
            }
        case .failure(let message):
            connectionError = message
        case .message(let message):
            apply(message)
        case .degraded(let diagnostics):
            decodeDiagnostics = decodeDiagnostics + diagnostics
            LinkTrace.shared.mark("conversation-degraded")
        }
    }

    private func apply(_ message: ConversationServerMessage) {
        switch message {
        case .snapshot(let snapshot):
            resyncRequestedAtRevision = nil
            let epochChanged = operationEpoch != nil && operationEpoch != snapshot.operationEpoch
            generation = snapshot.generation
            revision = snapshot.revision
            operationEpoch = snapshot.operationEpoch
            transcript.replaceAll(snapshot.items)
            _ = transcript.evict(maximumItems: maximumItems, maximumBytes: maximumBytes)
            renderedEnd = transcript.count
            state = snapshot.state
            pruneSettledResolveAttempts()
            serverHasMoreBefore = snapshot.hasMoreBefore
            if epochChanged || snapshot.reason == "operation_epoch" {
                markSendingOperationsForManualReview(reason: "The gateway operation record changed; review before retrying.")
            }
            mergeAcceptedOperations(with: snapshot.items)
            mergeOptimisticItems()
            // A snapshot replaces the transcript, so it is the one change
            // recorded as a whole cache rather than a journal entry.
            needsCompaction = true
            publishImmediately()
            flushPersistence()
            updateSocketPosition()
        case .itemsUpserted(let messageGeneration, let messageRevision, let upserts):
            guard acceptNextMutation(messageGeneration, revision: messageRevision) else { return }
            upsert(upserts)
            revision = messageRevision
            if mergeAcceptedOperations(with: upserts) { persistNow() } else { schedulePersist() }
            schedulePublish()
            updateSocketPosition()
        case .itemsRemoved(let messageGeneration, let messageRevision, let ids):
            guard acceptNextMutation(messageGeneration, revision: messageRevision) else { return }
            removeItems(Set(ids))
            revision = messageRevision
            schedulePersist()
            schedulePublish()
            updateSocketPosition()
        case .stateChanged(let messageGeneration, let messageRevision, let changedState):
            guard generation == messageGeneration else {
                requestResync()
                return
            }
            guard messageRevision >= revision else { return }
            if messageRevision > revision &+ 1 {
                // Tier-two overflow sends current state at a later revision.
                // Keep the useful availability state, but do not advance past
                // item mutations we have not applied; ask the Hub to replay or
                // snapshot from our last contiguous revision.
                state = changedState
                pruneSettledResolveAttempts()
                requestResync()
                schedulePersist()
                schedulePublish()
                return
            }
            state = changedState
            pruneSettledResolveAttempts()
            if messageRevision > revision {
                revision = messageRevision
                resyncRequestedAtRevision = nil
                updateSocketPosition()
            }
            schedulePersist()
            schedulePublish()
        case .operationResult(let operationID, let status, let itemID, let reason):
            applyOperationResult(operationID: operationID, status: status, itemID: itemID, reason: reason)
        case .historyPage(_, let page, let more):
            let oldFirst = items.first?.id
            let oldRenderedEnd = renderedEnd
            let oldCount = transcript.count
            upsert(page)
            serverHasMoreBefore = more
            // A history response moves the visible window toward the page.
            // Once full, the old first row remains visible for scroll anchoring.
            renderedEnd = oldCount < maximumRenderedItems
                ? transcript.count
                : min(transcript.count, oldRenderedEnd)
            prependAnchor = oldFirst
            publishImmediately()
            persistNow()
        case .error(_, let message):
            connectionError = message
        }
    }

    private func acceptNextMutation(_ messageGeneration: String, revision messageRevision: UInt64) -> Bool {
        guard generation == messageGeneration else {
            requestResync()
            return false
        }
        guard messageRevision > revision else { return false }
        guard messageRevision == revision &+ 1 else {
            requestResync()
            return false
        }
        resyncRequestedAtRevision = nil
        return true
    }

    private func upsert(_ newItems: [ConversationItem]) {
        guard !newItems.isEmpty else { return }
        preservingRenderedWindow {
            transcript.upsert(newItems)
            let evicted = transcript.evict(maximumItems: maximumItems, maximumBytes: maximumBytes)
            noteChanges(upserted: newItems.map(\.id), removed: evicted)
        }
    }

    private func removeItems(_ ids: Set<String>) {
        guard !ids.isEmpty else { return }
        preservingRenderedWindow {
            noteChanges(upserted: [], removed: transcript.remove(ids))
        }
    }

    /// Keeps a reader who is following the tail on it, and a reader looking
    /// at older rows on the same last row, across a transcript change.
    private func preservingRenderedWindow(_ mutate: () -> Void) {
        let oldLastID = items.last?.id
        let wasAtTail = renderedEnd == transcript.count
        mutate()
        if wasAtTail {
            renderedEnd = transcript.count
        } else if let oldLastID, let index = transcript.index(of: oldLastID) {
            renderedEnd = index + 1
        } else {
            renderedEnd = min(renderedEnd, transcript.count)
        }
    }

    private func appendOptimisticItem(for operation: ConversationOperation) {
        let local = ConversationItem(
            id: "operation:\(operation.id)",
            ordinal: UInt64.max - UInt64(operations.count),
            createdAt: ISO8601DateFormatter().string(from: operation.createdAt),
            kind: .message(role: "user", text: operation.text, status: .submitted)
        )
        upsert([local])
        publishImmediately()
    }

    private func mergeOptimisticItems() {
        for operation in operations where operation.status == .sending && operation.itemId == nil {
            let id = "operation:\(operation.id)"
            guard !transcript.contains(id) else { continue }
            upsert([ConversationItem(
                id: id,
                ordinal: UInt64.max - UInt64(operations.firstIndex(where: { $0.id == operation.id }) ?? 0),
                createdAt: ISO8601DateFormatter().string(from: operation.createdAt),
                kind: .message(role: "user", text: operation.text, status: .submitted)
            )])
        }
    }

    private func applyOperationResult(operationID: String, status: String, itemID: String?, reason: String?) {
        if resolveAttempts.contains(where: { $0.id == operationID }) {
            applyResolveResult(operationID: operationID, status: status, reason: reason)
            return
        }
        guard let index = operations.firstIndex(where: { $0.id == operationID }) else { return }
        switch status {
        case "accepted":
            operations[index].itemId = itemID
            // Keep the optimistic row until the canonical item is actually
            // observed. An accepted action precedes transcript observation and
            // removing it here would make the person's message blink away.
            if let itemID, transcript.contains(itemID) {
                removeItems(["operation:\(operationID)"])
                operations.remove(at: index)
            }
        case "refused":
            operations[index].status = .refused
            operations[index].reason = reason ?? "The host refused this message."
            // A refused message never reached the conversation, so it must
            // not stay drawn as though it had; the operation keeps its text.
            removeItems([operations[index].optimisticItemID])
        case "ambiguous":
            operations[index].status = .ambiguous
            operations[index].reason = reason ?? "It is unknown whether the host received this message."
        case "unknown":
            // The gateway retains no receipt for this id. It is not new work:
            // review it rather than send it again.
            operations[index].status = .manualReview
            operations[index].reason = reason ?? "The host has no record of this message; review before sending again."
        default:
            operations[index].status = .manualReview
            operations[index].reason = reason ?? "The host returned an unknown operation result."
        }
        publishImmediately()
        persistNow()
    }

    /// A refusal to apply an answer is an expected outcome: the request moved
    /// on, or the choice is no longer on screen. It is recorded, not raised.
    private func applyResolveResult(operationID: String, status: String, reason: String?) {
        switch status {
        case "accepted":
            updateResolveAttempt(operationID, status: .accepted, reason: nil)
            pruneSettledResolveAttempts()
        case "refused":
            updateResolveAttempt(operationID, status: .refused, reason: reason ?? "The host did not apply this answer.")
        case "ambiguous":
            updateResolveAttempt(operationID, status: .ambiguous, reason: reason ?? "It is unknown whether the host applied this answer.")
        default:
            updateResolveAttempt(operationID, status: .ambiguous, reason: reason ?? "The host has no record of this answer.")
        }
    }

    /// Returns whether any operation completed, so the caller records it now.
    @discardableResult
    private func mergeAcceptedOperations(with upserts: [ConversationItem]) -> Bool {
        let IDs = Set(upserts.map(\.id))
        var completed = operations.filter { $0.itemId.map(IDs.contains) == true }

        // The agent chooses the authoritative transcript id only after the kernel
        // accepts input, so an accepted result may have no correlation id.
        // Reconcile those submissions in order by exact normalized content
        // within the advertised retry window, as the architecture requires.
        var remaining = operations.filter {
            $0.status == .sending
                && $0.itemId == nil
                && Date.now.timeIntervalSince($0.createdAt) <= retentionSeconds
        }
        for item in upserts.sorted(by: { $0.ordinal < $1.ordinal }) {
            guard case .message(let role, let text, let status) = item.kind,
                  role == "user", status == .observed,
                  let match = remaining.firstIndex(where: {
                      $0.text.trimmingCharacters(in: .whitespacesAndNewlines)
                          == text.trimmingCharacters(in: .whitespacesAndNewlines)
                  })
            else { continue }
            completed.append(remaining.remove(at: match))
        }
        removeItems(Set(completed.map { "operation:\($0.id)" }))
        let completedIDs = Set(completed.map(\.id))
        operations.removeAll { completedIDs.contains($0.id) }
        return !completed.isEmpty
    }

    private func requestResync() {
        guard resyncRequestedAtRevision != revision else { return }
        resyncRequestedAtRevision = revision
        let generation = generation
        let revision = revision
        Task {
            do {
                try await socket?.send(.resume(generation: generation, afterRevision: revision))
            } catch let error as ConversationSocketError where error == .notConnected {
                // Reconnect already carries the same contiguous position on
                // the upgrade URL, so no extra retry is needed here.
            } catch {
                connectionError = error.localizedDescription
            }
        }
    }

    private func replayRetainedOperations() {
        let now = Date.now
        // Ambiguous outcomes are reconciled from the receipt, never redispatched.
        for operation in operations where operation.status == .ambiguous {
            Task { try? await socket?.send(.operationStatus(operationId: operation.id)) }
        }
        // An answer whose outcome was lost with the connection is asked
        // about, never sent again.
        for attempt in resolveAttempts where attempt.status == .sending || attempt.status == .ambiguous {
            Task { try? await socket?.send(.operationStatus(operationId: attempt.id)) }
        }
        for index in operations.indices where operations[index].status == .sending {
            guard now.timeIntervalSince(operations[index].createdAt) <= retentionSeconds else {
                operations[index].status = .manualReview
                operations[index].reason = "The retry window expired; review and send again with a new operation."
                continue
            }
            guard operations[index].operationEpoch == operationEpoch else {
                operations[index].status = .manualReview
                operations[index].reason = "The conversation operation epoch changed; review before retrying."
                continue
            }
            send(operation: operations[index])
        }
        persistNow()
    }

    private func send(operation: ConversationOperation) {
        Task {
            do {
                try await socket?.send(.sendMessage(
                    operationEpoch: operation.operationEpoch,
                    operationId: operation.id,
                    text: operation.text
                ))
            } catch let error as ConversationSocketError where error == .notConnected {
                // The reconnect path will replay this ID only while retention
                // permits it. It stays visible immediately either way.
            } catch {
                connectionError = error.localizedDescription
            }
        }
    }

    private func markSendingOperationsForManualReview(reason: String) {
        for index in operations.indices where operations[index].status == .sending {
            operations[index].status = .manualReview
            operations[index].reason = reason
        }
    }

    private func schedulePublish() {
        publishTask?.cancel()
        publishTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(16))
            guard !Task.isCancelled else { return }
            self?.publishImmediately()
        }
    }

    /// Publishes any change still waiting on the 16 ms coalescing tick.
    func publishPendingChanges() {
        publishImmediately()
    }

    private func publishImmediately() {
        publishTask?.cancel()
        publishTask = nil
        publishRenderedItems()
    }

    private func publishRenderedItems() {
        renderedEnd = min(renderedEnd, transcript.count)
        let window = Array(transcript.items[max(0, renderedEnd - maximumRenderedItems)..<renderedEnd])
        // An equal window is not reassigned: that would invalidate every
        // observer of `items` for a state-only change.
        if window != items { items = window }
        // Leave room for a whole 100-item wire page (each item is at most
        // 32 KiB). When capacity is exhausted, hide the remote paging control.
        let more = serverHasMoreBefore
            && transcript.count <= maximumItems - min(100, maximumItems)
            && transcript.totalBytes <= maximumBytes - min(100 * 32 * 1024, maximumBytes)
        if more != hasMoreBefore { hasMoreBefore = more }
    }

    private func updateSocketPosition() {
        guard let socket else { return }
        let position = resumePosition
        Task { await socket.updateResumePosition(position) }
    }

    private func noteChanges(upserted: [String], removed: [String]) {
        for id in upserted {
            removedItemIDs.remove(id)
            dirtyItemIDs.insert(id)
        }
        for id in removed {
            dirtyItemIDs.remove(id)
            removedItemIDs.insert(id)
        }
        hasUnpersistedChanges = true
    }

    /// Streaming changes are coalesced: the first one arms a single write and
    /// later ones join it, so a turn costs one small append per interval
    /// however fast its partial updates arrive. The loss window on a crash is
    /// one interval, and it is safe: an entry carries the revision its items
    /// belong to, so a lost entry only means resuming from an older revision.
    private func schedulePersist() {
        hasUnpersistedChanges = true
        guard persistTask == nil else { return }
        persistTask = Task { [weak self, persistInterval] in
            try? await Task.sleep(for: persistInterval)
            guard !Task.isCancelled else { return }
            self?.flushPersistence()
        }
    }

    /// Operation and history changes are written without waiting: operation
    /// records decide what may be retried, and a fetched page is not
    /// re-fetchable for free.
    private func persistNow() {
        hasUnpersistedChanges = true
        flushPersistence()
    }

    /// Appends one journal entry holding only the items changed since the last
    /// entry. The whole cache is rewritten only when a snapshot replaced the
    /// transcript, an append failed (a missing entry would break the
    /// contiguous journal), or the journal outgrew the transcript it describes;
    /// the last keeps replay cheaper than a rewrite and total bytes written
    /// linear in bytes changed.
    func flushPersistence() {
        persistTask?.cancel()
        persistTask = nil
        guard hasUnpersistedChanges || needsCompaction else { return }
        if !needsCompaction {
            let entry = ConversationJournalEntry(
                sequence: journalSequence &+ 1,
                generation: generation,
                revision: revision,
                operationEpoch: operationEpoch,
                state: state,
                hasMoreBefore: serverHasMoreBefore,
                operations: operations,
                upserts: dirtyItemIDs.compactMap(transcript.item(id:)).sorted { $0.ordinal < $1.ordinal },
                removedIDs: removedItemIDs.sorted()
            )
            do {
                let journalBytes = try storage.append(entry, sessionID: sessionID)
                journalSequence = entry.sequence
                clearPendingChanges()
                guard journalBytes > max(Self.minimumCompactionBytes, transcript.totalBytes) else { return }
            } catch {}
            needsCompaction = true
        }
        let cache = ConversationStoreCache(
            generation: generation,
            revision: revision,
            operationEpoch: operationEpoch,
            items: transcript.items,
            state: state,
            hasMoreBefore: serverHasMoreBefore,
            operations: operations,
            journalSequence: journalSequence
        )
        // On failure the flags stay set and the next flush retries the rewrite.
        guard (try? storage.save(cache, sessionID: sessionID)) != nil else { return }
        needsCompaction = false
        clearPendingChanges()
    }

    private func clearPendingChanges() {
        dirtyItemIDs.removeAll(keepingCapacity: true)
        removedItemIDs.removeAll(keepingCapacity: true)
        hasUnpersistedChanges = false
    }
}

/// The retained transcript in ordinal order, with an id index and a running
/// encoded-size total, so neither a lookup nor the byte budget needs a pass
/// over every item. Each item is measured once, when it enters or changes.
struct RetainedTranscript {
    private(set) var items: [ConversationItem] = []
    private(set) var totalBytes = 0
    private(set) var measuredItemCount = 0
    /// Absolute positions: index plus `evictedCount`, so evicting from the
    /// front does not rewrite every remaining entry.
    private var positions: [String: Int] = [:]
    private var sizes: [String: Int] = [:]
    private var evictedCount = 0
    private let encoder = JSONEncoder()

    var count: Int { items.count }

    func contains(_ id: String) -> Bool { positions[id] != nil }

    func index(of id: String) -> Int? { positions[id].map { $0 - evictedCount } }

    func item(id: String) -> ConversationItem? { index(of: id).map { items[$0] } }

    mutating func replaceAll(_ source: [ConversationItem]) {
        var byID: [String: ConversationItem] = [:]
        source.forEach { byID[$0.id] = $0 }
        items = byID.values.sorted { $0.ordinal < $1.ordinal }
        sizes.removeAll(keepingCapacity: true)
        totalBytes = 0
        for item in items { measure(item) }
        reindexAll()
    }

    /// A tail update replaces in place or appends. Only an item that lands
    /// before existing ones moves anything, and a batch of those (a history
    /// page) is placed with one sort.
    mutating func upsert(_ source: [ConversationItem]) {
        var moved = false
        var lateArrivals: [ConversationItem] = []
        for item in source {
            measure(item)
            if let index = index(of: item.id) {
                moved = moved || items[index].ordinal != item.ordinal
                items[index] = item
            } else {
                if let last = items.last, last.ordinal > item.ordinal { lateArrivals.append(item) }
                items.append(item)
                positions[item.id] = items.count - 1 + evictedCount
            }
        }
        guard moved || !lateArrivals.isEmpty else { return }
        if !moved, lateArrivals.count == 1, let item = lateArrivals.first, items.last?.id == item.id {
            // One late arrival behind the tail (typically ahead of optimistic
            // rows): shift only the rows after its place.
            items.removeLast()
            let target = items.partitioningIndex { $0.ordinal > item.ordinal }
            items.insert(item, at: target)
            reindex(from: target)
        } else {
            items.sort { $0.ordinal < $1.ordinal }
            reindexAll()
        }
    }

    @discardableResult
    mutating func remove(_ ids: Set<String>) -> [String] {
        let present = ids.compactMap { id in index(of: id).map { (id, $0) } }
        guard let first = present.map(\.1).min() else { return [] }
        items.removeAll { ids.contains($0.id) }
        for (id, _) in present {
            positions[id] = nil
            totalBytes -= sizes.removeValue(forKey: id) ?? 0
        }
        reindex(from: first)
        return present.map(\.0)
    }

    /// Drops the oldest rows until both bounds hold, keeping at least one.
    mutating func evict(maximumItems: Int, maximumBytes: Int) -> [String] {
        var dropped = 0
        var bytes = totalBytes
        while items.count - dropped > 1,
              items.count - dropped > maximumItems || bytes > maximumBytes {
            bytes -= sizes[items[dropped].id] ?? 0
            dropped += 1
        }
        guard dropped > 0 else { return [] }
        let ids = items[..<dropped].map(\.id)
        for id in ids {
            positions[id] = nil
            sizes[id] = nil
        }
        items.removeFirst(dropped)
        evictedCount += dropped
        totalBytes = bytes
        return ids
    }

    private mutating func measure(_ item: ConversationItem) {
        let size = (try? encoder.encode(item).count) ?? 0
        measuredItemCount += 1
        totalBytes += size - (sizes.updateValue(size, forKey: item.id) ?? 0)
    }

    private mutating func reindex(from start: Int) {
        for index in start..<items.count { positions[items[index].id] = index + evictedCount }
    }

    private mutating func reindexAll() {
        evictedCount = 0
        positions.removeAll(keepingCapacity: true)
        reindex(from: 0)
    }
}

private extension Array {
    /// The first index whose element satisfies `belongsInSecondPartition`,
    /// for an array already partitioned by it.
    func partitioningIndex(where belongsInSecondPartition: (Element) -> Bool) -> Int {
        var low = startIndex
        var high = endIndex
        while low < high {
            let middle = low + (high - low) / 2
            if belongsInSecondPartition(self[middle]) { high = middle } else { low = middle + 1 }
        }
        return low
    }
}
