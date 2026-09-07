import Foundation

/// Keeps this phone's offers clear of the Mac's agent hand-over.
///
/// The Mac answers offers with one pre-gathered ICE agent at a time. Presence
/// advertises that agent's ports; answering an offer consumes it, and the
/// replacement's ports reach presence only once the Mac has noticed the swap
/// and republished. An offer posted inside that window is answered by the
/// replacement while this phone runs its checks against the consumed agent's
/// ports — which now belong to a session in progress that ignores a stranger's
/// checks. The attempt ends in an ICE timeout that has nothing to do with the
/// network, and it is the ordinary case rather than a rare one: the transport
/// opens a fresh channel for every loopback connection, so a screen that
/// makes two requests posts two offers seconds apart.
///
/// Two rules keep the phone out of the window. Offers for one Mac are
/// posted one at a time, so parallel loopback connections cannot race each
/// other for the Mac's single idle agent. And before posting, the phone waits
/// until presence no longer describes the agent its last offer was answered
/// with. The wait is bounded: a Mac that never republishes is an older build
/// on its slow cadence, and offering anyway is still the right move — it is
/// only less likely to land.
public actor RendezvousSequencer {
    private nonisolated static let registry = PeerSequencers()

    /// Route rebuilds and separate providers on this phone share the Mac's one
    /// offer queue. Keep the last consumed agent across those rebuilds too.
    public nonisolated static func shared(for peerPublicKey: String) -> RendezvousSequencer {
        registry.sequencer(for: peerPublicKey)
    }

    /// The agent the last offer was answered with: enough of the answer to
    /// recognise the same agent in a later presence read, and nothing else.
    public struct AnsweredAgent: Equatable, Sendable {
        public let iceUfrag: String?
        public let candidates: [TransportCandidate]

        public init(iceUfrag: String?, candidates: [TransportCandidate]) {
            self.iceUfrag = iceUfrag
            self.candidates = candidates
        }

        /// Whether `presence` still advertises this agent.
        ///
        /// Compared by credentials and candidate addresses only. A presence
        /// refresh re-stamps every candidate's expiry without changing the
        /// agent behind it, and an agent is told apart from its replacement
        /// by its ports, which are ephemeral and never reused while the old
        /// agent is still answering.
        public func isDescribed(by presence: PeerPresence) -> Bool {
            presence.iceUfrag == iceUfrag
                && Set(presence.candidates.map(\.address)) == Set(candidates.map(\.address))
        }
    }

    /// How many presence reads, half a second apart, the wait spends before
    /// offering against whatever presence says. Five seconds covers a Mac
    /// that republishes as soon as its helper reports the replacement, with
    /// room for a slow status poll, and is short next to the connectivity
    /// checks it protects.
    public static let replacementReads = 10
    public static let replacementPoll: Duration = .milliseconds(500)

    private var tail: Task<Void, Never>?
    private var lastAnswered: AnsweredAgent?
    private let sleep: @Sendable (Duration) async throws -> Void

    /// - Parameter sleep: injected so the wait can be tested without spending
    ///   it.
    public init(
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.sleep = sleep
    }

    /// Runs `operation` after every operation enqueued before it has finished,
    /// whether that one returned or threw.
    public func serialized<T: Sendable>(
        _ operation: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        let previous = tail
        let task = Task<T, any Error> {
            await previous?.value
            try Task.checkCancellation()
            return try await operation()
        }
        tail = Task { _ = try? await task.value }
        return try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
    }

    /// Records what the Mac answered the latest offer with. `nil` when the
    /// answer carried no agent, in which case there is nothing to wait out
    /// next time.
    public func recordAnswer(_ agent: AnsweredAgent?) {
        lastAnswered = agent
    }

    /// The agent the latest offer was answered with, if any.
    public var answeredAgent: AnsweredAgent? { lastAnswered }

    /// Waits until presence describes a different agent from the one the last
    /// offer was answered with, or until the bounded wait runs out.
    ///
    /// A presence read that fails ends the wait rather than the attempt: the
    /// offer that follows makes the same request and reports the failure
    /// where it can be shown.
    public func awaitReplacement(
        readingPresence: @Sendable () async throws -> PeerPresence
    ) async {
        guard let lastAnswered else { return }
        for read in 0..<Self.replacementReads {
            guard let presence = try? await readingPresence() else { return }
            if !lastAnswered.isDescribed(by: presence) { return }
            if read + 1 < Self.replacementReads {
                do {
                    try await sleep(Self.replacementPoll)
                } catch {
                    return
                }
            }
        }
    }
}

private final class PeerSequencers: @unchecked Sendable {
    private let lock = NSLock()
    private var peers: [String: RendezvousSequencer] = [:]

    func sequencer(for key: String) -> RendezvousSequencer {
        lock.lock()
        defer { lock.unlock() }
        if let existing = peers[key] { return existing }
        let created = RendezvousSequencer()
        peers[key] = created
        return created
    }
}
