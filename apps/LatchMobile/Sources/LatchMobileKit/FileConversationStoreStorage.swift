import Foundation

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
