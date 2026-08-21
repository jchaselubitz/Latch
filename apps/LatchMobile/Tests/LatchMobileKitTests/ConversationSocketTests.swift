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

        func send(_ data: Data) async throws {}
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
}
