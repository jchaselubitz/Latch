import Foundation

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
