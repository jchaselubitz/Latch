import XCTest

@testable import LatchMobileKit

final class ConversationSocketTests: XCTestCase {
    private enum Closed: Error { case now }

    private final class OneMessageConnection: ConversationSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var data: [Data]

        init(data: Data) { self.data = [data] }

        func receive() async throws -> Data {
            let next: Data? = lock.withLock {
                guard !data.isEmpty else { return nil }
                return data.removeFirst()
            }
            guard let next else { throw Closed.now }
            return next
        }

        func send(_ text: String) async throws {}
        func cancel() {}
    }

    private final class RecordingConnection: ConversationSocketConnection, @unchecked Sendable {
        private let lock = NSLock()
        private var messages: [String] = []

        var sentMessages: [String] { lock.withLock { messages } }

        func receive() async throws -> Data {
            try await Task.sleep(for: .seconds(60))
            throw CancellationError()
        }

        func send(_ text: String) async throws {
            lock.withLock { messages.append(text) }
        }

        func cancel() {}
    }

    private actor Recorder {
        var positions: [ConversationResumePosition] = []
        var events: [ConversationSocketEvent] = []
        func record(position: ConversationResumePosition) { positions.append(position) }
        func record(event: ConversationSocketEvent) { events.append(event) }
    }

    func testServerFirstSnapshotUsesStoredUpgradePosition() async throws {
        let payload = Data("""
        {"type":"snapshot","generation":"g","revision":7,"operationEpoch":"e",\
        "items":[],"state":{"phase":"ready","sendMessage":{"enabled":true},\
        "resolveRequest":{"enabled":false},"pendingRequest":null,"connector":null},\
        "hasMoreBefore":false,"reason":"initial"}
        """.utf8)
        let recorder = Recorder()
        let socket = ConversationSocket(
            makeConnection: { position in
                await recorder.record(position: position)
                return OneMessageConnection(data: payload)
            },
            eventHandler: { event in await recorder.record(event: event) }
        )

        await socket.start(position: ConversationResumePosition(generation: "g", afterRevision: 6, operationEpoch: "e"))
        try await Task.sleep(for: .milliseconds(80))
        await socket.stop()

        let positions = await recorder.positions
        XCTAssertEqual(positions.first, ConversationResumePosition(generation: "g", afterRevision: 6, operationEpoch: "e"))
        let events = await recorder.events
        XCTAssertTrue(events.contains { event in
            if case .message(.snapshot(let snapshot)) = event { return snapshot.revision == 7 }
            return false
        })
    }

    func testClientMessageIsSentAsJSONText() async throws {
        let connection = RecordingConnection()
        let recorder = Recorder()
        let socket = ConversationSocket(
            makeConnection: { _ in connection },
            eventHandler: { event in await recorder.record(event: event) }
        )

        await socket.start(position: ConversationResumePosition())
        for _ in 0..<100 {
            if await recorder.events.contains(where: {
                if case .state(.open) = $0 { return true }
                return false
            }) { break }
            try await Task.sleep(for: .milliseconds(10))
        }

        try await socket.send(.operationStatus(operationId: "op-1"))
        await socket.stop()

        let sent = try XCTUnwrap(connection.sentMessages.first)
        let frame = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(sent.utf8)) as? [String: String])
        XCTAssertEqual(frame["type"], "operation_status")
        XCTAssertEqual(frame["operationId"], "op-1")
    }
}
