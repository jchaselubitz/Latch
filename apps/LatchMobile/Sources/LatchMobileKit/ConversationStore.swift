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
}

public protocol ConversationStoreStorage {
    func load(sessionID: String) throws -> ConversationStoreCache?
    func save(_ cache: ConversationStoreCache, sessionID: String) throws
}

public struct ConversationStoreCache: Codable, Equatable, Sendable {
    public var generation: String?
    public var revision: UInt64
    public var operationEpoch: String?
    public var items: [ConversationItem]
    public var state: ConversationState?
    public var hasMoreBefore: Bool
    public var operations: [ConversationOperation]

    public init(
        generation: String? = nil,
        revision: UInt64 = 0,
        operationEpoch: String? = nil,
        items: [ConversationItem] = [],
        state: ConversationState? = nil,
        hasMoreBefore: Bool = false,
        operations: [ConversationOperation] = []
    ) {
        self.generation = generation
        self.revision = revision
        self.operationEpoch = operationEpoch
        self.items = items
        self.state = state
        self.hasMoreBefore = hasMoreBefore
        self.operations = operations
    }
}

/// Disk-backed, per-session cache. Existing v1 derived event caches are not
/// consulted or migrated: v2 snapshots are a complete replacement boundary.
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
        let url = fileURL(sessionID)
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        return try decoder.decode(ConversationStoreCache.self, from: Data(contentsOf: url))
    }

    public func save(_ cache: ConversationStoreCache, sessionID: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try encoder.encode(cache).write(to: fileURL(sessionID), options: .atomic)
    }

    private func fileURL(_ sessionID: String) -> URL {
        let safe = sessionID.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) ? String($0) : "_" }.joined()
        return directory.appendingPathComponent("\(safe).json")
    }
}

@MainActor
@Observable
public final class ConversationStore {
    /// Rows currently offered to the view. The complete local history is in
    /// `retainedItems`; moving this window never evicts that history.
    public private(set) var items: [ConversationItem]
    public var retainedItems: [ConversationItem] { workingItems }
    public var hasEarlierRendered: Bool { renderedEnd > items.count }
    public var hasNewerRendered: Bool { renderedEnd < workingItems.count }
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

    public let sessionID: String
    private let storage: any ConversationStoreStorage
    private let maximumItems: Int
    private let maximumBytes: Int
    private let maximumRenderedItems = 300
    private var gateway: LatchGateway
    private var retentionSeconds: TimeInterval
    private var socket: ConversationSocket?
    private var workingItems: [ConversationItem]
    private var renderedEnd: Int
    private var serverHasMoreBefore: Bool
    private var publishTask: Task<Void, Never>?
    private var isStarted = false
    private var resyncRequestedAtRevision: UInt64?

    public init(
        sessionID: String,
        gateway: LatchGateway,
        operationRetentionSeconds: Int,
        storage: any ConversationStoreStorage = FileConversationStoreStorage(),
        maximumItems: Int = 10_000,
        maximumBytes: Int = 32 * 1024 * 1024
    ) {
        self.sessionID = sessionID
        self.gateway = gateway
        retentionSeconds = TimeInterval(max(0, operationRetentionSeconds))
        self.storage = storage
        self.maximumItems = maximumItems
        self.maximumBytes = maximumBytes
        let cached = (try? storage.load(sessionID: sessionID)) ?? ConversationStoreCache()
        generation = cached.generation
        revision = cached.revision
        operationEpoch = cached.operationEpoch
        let restoredItems = Self.bounded(cached.items, maximumItems: maximumItems, maximumBytes: maximumBytes)
        workingItems = restoredItems
        renderedEnd = restoredItems.count
        items = Array(restoredItems.suffix(maximumRenderedItems))
        state = cached.state
        serverHasMoreBefore = cached.hasMoreBefore
        hasMoreBefore = cached.hasMoreBefore && restoredItems.count < maximumItems
        operations = cached.operations
        publishRenderedItems()
    }

    public var canSend: Bool { state?.sendMessage.enabled == true && operationEpoch != nil }
    public var sendReason: String? { state?.sendMessage.reason ?? (operationEpoch == nil ? "Waiting for conversation state" : nil) }
    public var canResolve: Bool { state?.resolveRequest.enabled == true && operationEpoch != nil }
    public var resolveReason: String? { state?.resolveRequest.reason }
    public var pendingRequest: ConversationItem? {
        guard let requestID = state?.pendingRequest else { return nil }
        return workingItems.first { item in
            if case .request(let id, _, _, _, _) = item.kind { return id == requestID }
            return false
        }
    }

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
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, canSend, let operationEpoch else { return }
        let operation = ConversationOperation(id: UUID().uuidString, text: text, operationEpoch: operationEpoch)
        operations.append(operation)
        appendOptimisticItem(for: operation)
        persist()
        send(operation: operation)
    }

    /// An explicit retry is always a new operation. In particular, ambiguous
    /// operations may already have reached the kernel and must never be replayed.
    public func retry(_ operationID: String) {
        guard let operation = operations.first(where: { $0.id == operationID }) else { return }
        send(text: operation.text)
    }

    public func resolve(requestID: String, choice: String) {
        guard canResolve, let operationEpoch else { return }
        let operationID = UUID().uuidString
        Task {
            do {
                try await socket?.send(.resolveRequest(
                    operationEpoch: operationEpoch,
                    operationId: operationID,
                    requestId: requestID,
                    choice: choice
                ))
            } catch {
                connectionError = error.localizedDescription
            }
        }
    }

    public func loadOlder(limit: Int = 100) {
        if hasEarlierRendered {
            prependAnchor = items.first?.id
            renderedEnd = max(maximumRenderedItems, renderedEnd - min(100, max(1, limit)))
            publishRenderedItems()
            return
        }
        guard hasMoreBefore, let oldest = workingItems.map(\.ordinal).filter({ $0 != UInt64.max }).min() else { return }
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
        renderedEnd = min(workingItems.count, renderedEnd + 100)
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
            workingItems = Self.bounded(snapshot.items, maximumItems: maximumItems, maximumBytes: maximumBytes)
            renderedEnd = workingItems.count
            state = snapshot.state
            serverHasMoreBefore = snapshot.hasMoreBefore
            if epochChanged || snapshot.reason == "operation_epoch" {
                markSendingOperationsForManualReview(reason: "The gateway operation record changed; review before retrying.")
            }
            mergeAcceptedOperations(with: snapshot.items)
            mergeOptimisticItems()
            publishImmediately()
            updateSocketPosition()
        case .itemsUpserted(let messageGeneration, let messageRevision, let upserts):
            guard acceptNextMutation(messageGeneration, revision: messageRevision) else { return }
            upsert(upserts)
            revision = messageRevision
            mergeAcceptedOperations(with: upserts)
            schedulePublish()
            updateSocketPosition()
        case .itemsRemoved(let messageGeneration, let messageRevision, let ids):
            guard acceptNextMutation(messageGeneration, revision: messageRevision) else { return }
            let oldLastID = items.last?.id
            let wasAtTail = renderedEnd == workingItems.count
            workingItems.removeAll { ids.contains($0.id) }
            if wasAtTail {
                renderedEnd = workingItems.count
            } else if let oldLastID, let index = workingItems.firstIndex(where: { $0.id == oldLastID }) {
                renderedEnd = index + 1
            }
            revision = messageRevision
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
                requestResync()
                schedulePublish()
                return
            }
            state = changedState
            if messageRevision > revision {
                revision = messageRevision
                resyncRequestedAtRevision = nil
                updateSocketPosition()
            }
            schedulePublish()
        case .operationResult(let operationID, let status, let itemID, let reason):
            applyOperationResult(operationID: operationID, status: status, itemID: itemID, reason: reason)
        case .historyPage(_, let page, let more):
            let oldFirst = items.first?.id
            let oldRenderedEnd = renderedEnd
            let oldCount = workingItems.count
            upsert(page)
            serverHasMoreBefore = more
            // A history response moves the visible window toward the page.
            // Once full, the old first row remains visible for scroll anchoring.
            renderedEnd = oldCount < maximumRenderedItems
                ? workingItems.count
                : min(workingItems.count, oldRenderedEnd)
            prependAnchor = oldFirst
            publishImmediately()
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
        let oldLastID = items.last?.id
        let wasAtTail = renderedEnd == workingItems.count
        var byID = Dictionary(uniqueKeysWithValues: workingItems.map { ($0.id, $0) })
        newItems.forEach { byID[$0.id] = $0 }
        workingItems = Self.bounded(Array(byID.values), maximumItems: maximumItems, maximumBytes: maximumBytes)
        if wasAtTail {
            renderedEnd = workingItems.count
        } else if let oldLastID, let index = workingItems.firstIndex(where: { $0.id == oldLastID }) {
            renderedEnd = index + 1
        } else {
            renderedEnd = min(renderedEnd, workingItems.count)
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
        let existing = Set(workingItems.map(\.id))
        for operation in operations where operation.status == .sending && operation.itemId == nil {
            let id = "operation:\(operation.id)"
            guard !existing.contains(id) else { continue }
            upsert([ConversationItem(
                id: id,
                ordinal: UInt64.max - UInt64(operations.firstIndex(where: { $0.id == operation.id }) ?? 0),
                createdAt: ISO8601DateFormatter().string(from: operation.createdAt),
                kind: .message(role: "user", text: operation.text, status: .submitted)
            )])
        }
    }

    private func applyOperationResult(operationID: String, status: String, itemID: String?, reason: String?) {
        guard let index = operations.firstIndex(where: { $0.id == operationID }) else { return }
        switch status {
        case "accepted":
            operations[index].itemId = itemID
            // Keep the optimistic row until the canonical item is actually
            // observed. An accepted action precedes transcript observation and
            // removing it here would make the person's message blink away.
            if let itemID, workingItems.contains(where: { $0.id == itemID }) {
                workingItems.removeAll { $0.id == "operation:\(operationID)" }
                operations.remove(at: index)
            }
        case "refused":
            operations[index].status = .refused
            operations[index].reason = reason ?? "The host refused this message."
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
    }

    private func mergeAcceptedOperations(with upserts: [ConversationItem]) {
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
        for operation in completed {
            workingItems.removeAll { $0.id == "operation:\(operation.id)" }
        }
        let completedIDs = Set(completed.map(\.id))
        operations.removeAll { completedIDs.contains($0.id) }
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
        persist()
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

    private func publishImmediately() {
        publishTask?.cancel()
        publishTask = nil
        publishRenderedItems()
        persist()
    }

    private func publishRenderedItems() {
        renderedEnd = min(renderedEnd, workingItems.count)
        items = Array(workingItems[max(0, renderedEnd - maximumRenderedItems)..<renderedEnd])
        // Leave room for a whole 100-item wire page (each item is at most
        // 32 KiB). When capacity is exhausted, hide the remote paging control.
        let size = (try? JSONEncoder().encode(workingItems).count) ?? maximumBytes
        hasMoreBefore = serverHasMoreBefore
            && workingItems.count <= maximumItems - min(100, maximumItems)
            && size <= maximumBytes - min(100 * 32 * 1024, maximumBytes)
    }

    private func updateSocketPosition() {
        guard let socket else { return }
        let position = resumePosition
        Task { await socket.updateResumePosition(position) }
    }

    private func persist() {
        let cache = ConversationStoreCache(
            generation: generation,
            revision: revision,
            operationEpoch: operationEpoch,
            items: workingItems,
            state: state,
            hasMoreBefore: serverHasMoreBefore,
            operations: operations
        )
        try? storage.save(cache, sessionID: sessionID)
    }

    private static func bounded(_ source: [ConversationItem], maximumItems: Int, maximumBytes: Int) -> [ConversationItem] {
        var byID: [String: ConversationItem] = [:]
        source.forEach { byID[$0.id] = $0 }
        var ordered = Array(byID.values).sorted { $0.ordinal < $1.ordinal }
        if ordered.count > maximumItems { ordered.removeFirst(ordered.count - maximumItems) }
        let encoder = JSONEncoder()
        while ordered.count > 1, (try? encoder.encode(ordered).count) ?? 0 > maximumBytes {
            ordered.removeFirst()
        }
        return ordered
    }
}
