import Foundation

// MARK: - Persistence

extension ConversationStore {
    /// Below this the journal is never compacted: rewriting a small cache to
    /// save replaying a few entries is not worth the write.
    private static let minimumCompactionBytes = 256 * 1024

    func noteChanges(upserted: [String], removed: [String]) {
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
    func schedulePersist() {
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
    func persistNow() {
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
