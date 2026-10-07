import Foundation

// MARK: - Transcript

extension ConversationStore {
    public var retainedItems: [ConversationItem] { transcript.items }

    public var hasEarlierRendered: Bool { renderedEnd > items.count }

    public var hasNewerRendered: Bool { renderedEnd < transcript.count }

    public var isHistoryLimitReached: Bool { serverHasMoreBefore && !hasMoreBefore }

    /// Items measured for the byte budget over this store's lifetime. Each
    /// item is measured once when it enters or changes, never per publish.
    var measuredItemCount: Int { transcript.measuredItemCount }

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

    func upsert(_ newItems: [ConversationItem]) {
        guard !newItems.isEmpty else { return }
        preservingRenderedWindow {
            transcript.upsert(newItems)
            let evicted = transcript.evict(maximumItems: maximumItems, maximumBytes: maximumBytes)
            noteChanges(upserted: newItems.map(\.id), removed: evicted)
        }
    }

    func removeItems(_ ids: Set<String>) {
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

    func schedulePublish() {
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

    func publishImmediately() {
        publishTask?.cancel()
        publishTask = nil
        publishRenderedItems()
    }

    func publishRenderedItems() {
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
}
