import Foundation

/// Each helper must deliver events independently. FileHandle.bytes uses a
/// shared blocking reader on macOS, so an idle session can stall enrollment
/// and events from every other helper.
final class HelperLineReader: @unchecked Sendable {
    let lines: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let handle: FileHandle
    private let lock = NSLock()
    private var buffer = Data()
    private var finished = false

    init(handle: FileHandle) {
        self.handle = handle
        var continuation: AsyncStream<String>.Continuation!
        self.lines = AsyncStream { continuation = $0 }
        self.continuation = continuation
        handle.readabilityHandler = { [weak self] _ in
            self?.readAvailableData()
        }
    }

    private func readAvailableData() {
        lock.withLock {
            guard !finished else { return }
            let data = handle.availableData
            if data.isEmpty {
                if !buffer.isEmpty {
                    continuation.yield(String(decoding: buffer, as: UTF8.self))
                    buffer.removeAll()
                }
                finished = true
                handle.readabilityHandler = nil
                continuation.finish()
                return
            }
            buffer.append(data)
            while let newline = buffer.firstIndex(of: 0x0a) {
                var line = buffer[..<newline]
                if line.last == 0x0d { line = line.dropLast() }
                continuation.yield(String(decoding: line, as: UTF8.self))
                buffer.removeSubrange(...newline)
            }
        }
    }

    func stop() {
        lock.withLock {
            finished = true
            handle.readabilityHandler = nil
            buffer.removeAll()
            continuation.finish()
        }
    }

    deinit { stop() }
}
