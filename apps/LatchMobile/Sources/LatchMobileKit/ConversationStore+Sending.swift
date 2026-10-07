import Foundation

// MARK: - Sending

extension ConversationStore {
    public var canSend: Bool { state?.sendMessage.enabled == true && operationEpoch != nil }

    /// The agent's own reason for closing its newest turn, while no turn is
    /// open: `answer`, `aborted`, `refusal`, or `error`. Nil when unknown.
    public var turnOutcome: String? { state?.turnOutcome }

    /// Slash commands the agent advertises. Empty when the Hub does not know
    /// them, so a composer may only offer what is listed, never assume.
    public var commands: [AdvertisedCommand] { state?.commands ?? [] }

    public var sendReason: String? { state?.sendMessage.reason ?? (operationEpoch == nil ? "Waiting for conversation state" : nil) }

    public func send(text: String) {
        guard attachments.isEmpty else {
            sendWithAttachments(text: text)
            return
        }
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, canSend, let operationEpoch else { return }
        enqueue(text: text, operationEpoch: operationEpoch)
    }

    func enqueue(text: String, operationEpoch: String) {
        let operation = ConversationOperation(id: UUID().uuidString, text: text, operationEpoch: operationEpoch)
        operations.append(operation)
        appendOptimisticItem(for: operation)
        persistNow()
        send(operation: operation)
    }

    /// An explicit retry is always a new operation. In particular, ambiguous
    /// operations may already have reached the kernel and must never be replayed.
    /// The settled record it replaces is dismissed, so one explicit choice
    /// cannot be offered, and taken, twice.
    public func retry(_ operationID: String) {
        guard let operation = operations.first(where: { $0.id == operationID }),
              operation.status != .sending, operation.status != .queued,
              canSend
        else { return }
        dismissOperation(operationID)
        send(text: operation.text)
    }

    /// Forgets a settled operation the person has reviewed, along with its
    /// transcript row. One still sending stays: its outcome is still coming.
    public func dismissOperation(_ operationID: String) {
        guard let index = operations.firstIndex(where: { $0.id == operationID }),
              operations[index].status != .sending && operations[index].status != .queued
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
              operation.status != .sending && operation.status != .queued
        else { return }
        draft = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? operation.text
            : draft + "\n\n" + operation.text
        dismissOperation(operationID)
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

    func mergeOptimisticItems() {
        for operation in operations where (operation.status == .sending || operation.status == .queued) && (operation.itemId.map { !transcript.contains($0) } ?? true) {
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

    func applyOperationResult(operationID: String, status: String, itemID: String?, reason: String?) {
        if cancelAttempt?.id == operationID {
            switch status {
            case "accepted": cancelAttempt?.status = .accepted
            case "refused": cancelAttempt?.status = .refused
            default: cancelAttempt?.status = .ambiguous
            }
            cancelAttempt?.reason = reason
            pruneSettledResolveAttempts()
            return
        }
        if resolveAttempts.contains(where: { $0.id == operationID }) {
            applyResolveResult(operationID: operationID, status: status, reason: reason)
            return
        }
        guard let index = operations.firstIndex(where: { $0.id == operationID }) else { return }
        switch status {
        case "accepted", "queued":
            if status == "queued" { operations[index].status = .queued }
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

    /// Returns whether any operation completed, so the caller records it now.
    @discardableResult
    func mergeAcceptedOperations(with upserts: [ConversationItem]) -> Bool {
        let IDs = Set(upserts.map(\.id))
        var completed = operations.filter { $0.itemId.map(IDs.contains) == true }

        // The agent chooses the authoritative transcript id only after the kernel
        // accepts input, so an accepted result may have no correlation id.
        // Reconcile those submissions in order by exact normalized content
        // within the advertised retry window, as the architecture requires.
        var remaining = operations.filter {
            ($0.status == .sending || $0.status == .queued)
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

    func replayRetainedOperations() {
        let now = Date.now
        // Ambiguous outcomes are reconciled from the receipt, never redispatched.
        if let attempt = cancelAttempt, attempt.isInFlight || attempt.status == .ambiguous {
            Task { try? await socket?.send(.operationStatus(operationId: attempt.id)) }
        }
        for operation in operations where operation.status == .ambiguous || operation.status == .queued {
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

    func markSendingOperationsForManualReview(reason: String) {
        for index in operations.indices where operations[index].status == .sending {
            operations[index].status = .manualReview
            operations[index].reason = reason
        }
    }
}
