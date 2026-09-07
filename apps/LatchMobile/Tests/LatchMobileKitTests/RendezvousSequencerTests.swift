import XCTest

@testable import LatchMobileKit

/// The Mac answers with one pre-gathered agent and presence lags behind the
/// swap. These cover the two rules that keep a phone's offers out of that
/// window: one offer at a time, and none against an agent it already used.
final class RendezvousSequencerTests: XCTestCase {
    private func candidate(_ address: String, expiresAt: UInt64 = 100) -> TransportCandidate {
        TransportCandidate(
            address: address,
            expiresAt: expiresAt,
            type: "srflx",
            priority: 1_694_498_815,
            foundation: "1",
            component: 1,
            protocol: "udp"
        )
    }

    private func presence(
        ufrag: String,
        addresses: [String],
        expiresAt: UInt64 = 100
    ) -> PeerPresence {
        PeerPresence(
            deviceId: "dev_mac",
            online: true,
            candidates: addresses.map { candidate($0, expiresAt: expiresAt) },
            iceUfrag: ufrag,
            icePwd: "a-short-term-stun-credential"
        )
    }

    func testProvidersForOneMacShareSequencingAndRememberTheConsumedAgent() async {
        let key = UUID().uuidString
        let first = RendezvousSequencer.shared(for: key)
        let second = RendezvousSequencer.shared(for: key)
        XCTAssertTrue(first === second)
        XCTAssertFalse(first === RendezvousSequencer.shared(for: UUID().uuidString))
        let agent = RendezvousSequencer.AnsweredAgent(iceUfrag: "consumed", candidates: [])
        await first.recordAnswer(agent)
        let remembered = await second.answeredAgent
        XCTAssertEqual(remembered, agent)
    }

    func testOffersFromOneRouteArePostedOneAtATime() async throws {
        let key = UUID().uuidString
        let sequencer = RendezvousSequencer.shared(for: key)
        let otherProvider = RendezvousSequencer.shared(for: key)
        let order = Recorder()
        let firstMayFinish = Gate()

        let first = Task {
            try await sequencer.serialized {
                await order.note("first started")
                await firstMayFinish.wait()
                await order.note("first finished")
            }
        }
        // The second is enqueued while the first is parked inside its work.
        await order.waitUntil(count: 1)
        let second = Task {
            try await otherProvider.serialized {
                await order.note("second started")
            }
        }
        // Give the second every chance to jump the queue before releasing
        // the first.
        await Task.yield()
        do { let observed = await order.notes; XCTAssertEqual(observed, ["first started"]) }

        await firstMayFinish.open()
        try await first.value
        try await second.value
        let notes = await order.notes
        XCTAssertEqual(notes, ["first started", "first finished", "second started"])
    }

    func testCancellingAQueuedOpenNeverPostsItsOffer() async throws {
        let sequencer = RendezvousSequencer()
        let order = Recorder()
        let firstMayFinish = Gate()
        let first = Task {
            try await sequencer.serialized {
                await order.note("first")
                await firstMayFinish.wait()
            }
        }
        await order.waitUntil(count: 1)
        let cancelled = Task {
            try await sequencer.serialized { await order.note("cancelled offer") }
        }
        cancelled.cancel()
        await firstMayFinish.open()
        try await first.value
        do {
            try await cancelled.value
            XCTFail("cancelled opening ran")
        } catch is CancellationError {}
        try await sequencer.serialized { await order.note("next") }
        let observed = await order.notes
        XCTAssertEqual(observed, ["first", "next"])
    }

    func testAFailedOfferStillReleasesTheNextOne() async throws {
        struct Failed: Error {}
        let sequencer = RendezvousSequencer()
        do {
            try await sequencer.serialized { throw Failed() }
            XCTFail("the failure was swallowed")
        } catch is Failed {}
        let value = try await sequencer.serialized { 7 }
        XCTAssertEqual(value, 7)
    }

    func testTheNextOfferWaitsUntilPresenceDescribesAReplacementAgent() async {
        let sleeps = Recorder()
        let sequencer = RendezvousSequencer(sleep: { duration in
            await sleeps.note("\(duration)")
        })
        let consumed = RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000"), candidate("192.168.1.20:52000")]
        )
        await sequencer.recordAnswer(consumed)

        // The Mac republishes on the third read. Presence lists the same
        // addresses in a different order until then, which is still the
        // same agent.
        let reads = Recorder()
        let replaced = presence(ufrag: "abc123", addresses: ["203.0.113.9:52007", "192.168.1.20:52007"])
        await sequencer.awaitReplacement {
            await reads.note("read")
            return await reads.notes.count < 3
                ? self.presence(ufrag: "abc123", addresses: ["192.168.1.20:52000", "203.0.113.9:52000"])
                : replaced
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, 3) }
        do { let observed = await sleeps.notes.count; XCTAssertEqual(observed, 2, "the wait slept a different number of times than it re-read") }
    }

    func testARefreshedRecordOfTheSameAgentIsNotAReplacement() async {
        let sequencer = RendezvousSequencer(sleep: { _ in })
        let consumed = RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000", expiresAt: 100)]
        )
        await sequencer.recordAnswer(consumed)
        let reads = Recorder()
        // The same ports with a later expiry is a presence refresh, not a new
        // agent; only a new set of ports or new credentials is.
        await sequencer.awaitReplacement {
            await reads.note("read")
            return self.presence(ufrag: "abc123", addresses: ["203.0.113.9:52000"], expiresAt: 190)
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, RendezvousSequencer.replacementReads) }
    }

    func testNewCredentialsAloneMeanAReplacementAgent() async {
        // A restarted helper mints new credentials and may, on a quiet
        // machine, land on the same ports again.
        let sequencer = RendezvousSequencer(sleep: { _ in })
        await sequencer.recordAnswer(RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000")]
        ))
        let reads = Recorder()
        await sequencer.awaitReplacement {
            await reads.note("read")
            return self.presence(ufrag: "def456", addresses: ["203.0.113.9:52000"])
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, 1) }
    }

    func testTheWaitIsBoundedWhenTheMacNeverRepublishes() async {
        let sleeps = Recorder()
        let sequencer = RendezvousSequencer(sleep: { _ in await sleeps.note("slept") })
        await sequencer.recordAnswer(RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000")]
        ))
        let reads = Recorder()
        await sequencer.awaitReplacement {
            await reads.note("read")
            return self.presence(ufrag: "abc123", addresses: ["203.0.113.9:52000"])
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, RendezvousSequencer.replacementReads) }
        do { let observed = await sleeps.notes.count; XCTAssertEqual(observed, RendezvousSequencer.replacementReads - 1) }
    }

    func testTheFirstOfferAndAnAgentlessAnswerNeverWait() async {
        let sequencer = RendezvousSequencer(sleep: { _ in })
        let reads = Recorder()
        await sequencer.awaitReplacement {
            await reads.note("read")
            return self.presence(ufrag: "abc123", addresses: ["203.0.113.9:52000"])
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, 0, "a phone with no answer yet read presence for nothing") }

        await sequencer.recordAnswer(RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000")]
        ))
        await sequencer.recordAnswer(nil)
        await sequencer.awaitReplacement {
            await reads.note("read")
            return self.presence(ufrag: "abc123", addresses: ["203.0.113.9:52000"])
        }
        do { let observed = await reads.notes.count; XCTAssertEqual(observed, 0) }
    }

    func testAPresenceReadFailureEndsTheWaitNotTheAttempt() async {
        struct Unreachable: Error {}
        let sleeps = Recorder()
        let sequencer = RendezvousSequencer(sleep: { _ in await sleeps.note("slept") })
        await sequencer.recordAnswer(RendezvousSequencer.AnsweredAgent(
            iceUfrag: "abc123",
            candidates: [candidate("203.0.113.9:52000")]
        ))
        await sequencer.awaitReplacement { throw Unreachable() }
        do { let observed = await sleeps.notes.count; XCTAssertEqual(observed, 0) }
    }
}

private actor Recorder {
    private(set) var notes: [String] = []
    private var waiters: [(count: Int, continuation: CheckedContinuation<Void, Never>)] = []

    func note(_ note: String) {
        notes.append(note)
        let ready = waiters.filter { $0.count <= notes.count }
        waiters.removeAll { $0.count <= notes.count }
        for waiter in ready { waiter.continuation.resume() }
    }

    func waitUntil(count: Int) async {
        if notes.count >= count { return }
        await withCheckedContinuation { continuation in
            waiters.append((count, continuation))
        }
    }
}

private actor Gate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        let released = waiters
        waiters = []
        for waiter in released { waiter.resume() }
    }
}
