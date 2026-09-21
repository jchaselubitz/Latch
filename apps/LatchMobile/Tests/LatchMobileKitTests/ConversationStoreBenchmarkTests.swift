import XCTest

@testable import LatchMobileKit

/// Cost of one streaming partial-message upsert, from `receive` through the
/// published tail and whatever persistence that publish performs. Opt-in:
/// `LATCH_STORE_BENCH=1 swift test --filter ConversationStoreBenchmarkTests`.
@MainActor
final class ConversationStoreBenchmarkTests: XCTestCase {
    func testPartialUpsertCost() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["LATCH_STORE_BENCH"] == "1")
        for count in [300, 2_000, 10_000] {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            defer { try? FileManager.default.removeItem(at: directory) }
            let store = ConversationStore(
                sessionID: "bench", gateway: LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: "")),
                operationRetentionSeconds: 60, storage: FileConversationStoreStorage(directory: directory)
            )
            let body = String(repeating: "Lorem ipsum dolor sit amet. ", count: 8)
            let items = (1...count).map {
                ConversationItem(id: "m\($0)", ordinal: UInt64($0), createdAt: "2026-09-21T00:00:00Z",
                                 kind: .message(role: "assistant", text: body, status: .complete))
            }
            let state = ConversationState(
                phase: "ready", sendMessage: OperationAvailability(enabled: true, reason: nil),
                resolveRequest: OperationAvailability(enabled: false, reason: nil), pendingRequest: nil,
                connector: ConnectorIdentity(id: "claude", version: "1")
            )
            store.receive(.message(.snapshot(ConversationSnapshot(
                generation: "g", revision: 1, operationEpoch: "e", items: items,
                state: state, hasMoreBefore: false, reason: "initial"
            ))))
            var revision: UInt64 = 1
            var tail = ""
            var samples: [Double] = []
            let flushEvery = 30 // ~500 ms of 16 ms publishes
            var flushSamples: [Double] = []
            for step in 0..<120 {
                revision += 1
                tail += "token\(step) "
                let partial = ConversationItem(id: "tail", ordinal: UInt64(count + 1), createdAt: "2026-09-21T00:00:00Z",
                                               kind: .message(role: "assistant", text: tail, status: .partial))
                let start = DispatchTime.now().uptimeNanoseconds
                store.receive(.message(.itemsUpserted(generation: "g", revision: revision, items: [partial])))
                store.publishPendingChanges()
                samples.append(Double(DispatchTime.now().uptimeNanoseconds - start) / 1e6)
                XCTAssertEqual(store.items.last?.id, "tail")
                if step % flushEvery == flushEvery - 1 {
                    let flushStart = DispatchTime.now().uptimeNanoseconds
                    store.flushPersistence()
                    flushSamples.append(Double(DispatchTime.now().uptimeNanoseconds - flushStart) / 1e6)
                }
            }
            samples.sort(); flushSamples.sort()
            let median = samples[samples.count / 2], p95 = samples[samples.count * 95 / 100]
            let flush = flushSamples.isEmpty ? 0 : flushSamples[flushSamples.count / 2]
            print(String(format: "BENCH retained=%d upsert→publish median=%.3fms p95=%.3fms throttled-flush median=%.3fms", count, median, p95, flush))
        }
    }
}
