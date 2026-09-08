import Foundation

/// Forwards the gateway's spooled attention events to the control plane.
///
/// The gateway writes one content-free entry per Hub transition it noticed
/// for a watched session; this poll reads them through the `latch` CLI, maps
/// the Mac-local device id to the paired phone's control-plane id, asks the
/// control plane for one generic alert, and acknowledges the entry. An entry
/// whose device is unknown or unpaired is acknowledged without a request:
/// there is nobody to tell. Delivery failures leave the entry for the next
/// poll until the gateway's own spool expiry discards it, so a control-plane
/// outage costs at most a late alert, never a stuck spool.
///
/// Everything here is best effort. The phone refreshes on foreground whether
/// or not a notification ever arrived.
final class AttentionForwarder: @unchecked Sendable {
    struct Outcome: Equatable {
        var forwarded = 0
        var skipped = 0
        var retained = 0
    }

    typealias Fetch = @Sendable () async throws -> [RemoteAttentionEvent]
    typealias Acknowledge = @Sendable (String) async throws -> Void
    typealias DirectoryID = @Sendable (String) async -> String?
    typealias Notify = @Sendable (String, String) async throws -> Void

    static let interval: Duration = .seconds(2)

    private let fetch: Fetch
    private let acknowledge: Acknowledge
    private let directoryID: DirectoryID
    private let notify: Notify

    init(fetch: @escaping Fetch, acknowledge: @escaping Acknowledge, directoryID: @escaping DirectoryID, notify: @escaping Notify) {
        self.fetch = fetch
        self.acknowledge = acknowledge
        self.directoryID = directoryID
        self.notify = notify
    }

    /// One pass over the spool.
    @discardableResult
    func forwardOnce() async -> Outcome {
        var outcome = Outcome()
        guard let events = try? await fetch() else { return outcome }
        for event in events {
            guard let clientID = await directoryID(event.deviceID) else {
                try? await acknowledge(event.eventID)
                outcome.skipped += 1
                continue
            }
            do {
                try await notify(clientID, event.eventID)
                try? await acknowledge(event.eventID)
                outcome.forwarded += 1
            } catch let error as ControlPlaneHostError where error.isPermanentAttentionRefusal {
                // The control plane will never accept this event: the pairing
                // is gone, the id is malformed, or it already has it.
                try? await acknowledge(event.eventID)
                outcome.skipped += 1
            } catch {
                outcome.retained += 1
            }
        }
        return outcome
    }

    /// Polls until cancelled.
    func run() async {
        while !Task.isCancelled {
            await forwardOnce()
            try? await Task.sleep(for: Self.interval)
        }
    }
}

extension ControlPlaneHostError {
    /// A refusal that a retry cannot change.
    var isPermanentAttentionRefusal: Bool {
        guard case .http(let status, _, _) = self else { return false }
        return status == 400 || status == 403 || status == 404
    }
}
