import Foundation

// MARK: - Connection

extension ConversationStore {
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

    private func updateSocketPosition() {
        guard let socket else { return }
        let position = resumePosition
        Task { await socket.updateResumePosition(position) }
    }
}
