import Foundation

/// Bounded restart backoff shared by every remote-access helper loop, so the
/// gateway, link, and admission-retry schedules cannot drift apart.
struct CrashLoopBackoff {
    private static let delays: [UInt64] = [1, 2, 5, 10, 30]
    /// A helper that stayed up this long before exiting was not crash-looping;
    /// its next restart starts the schedule over. Without this, every helper
    /// exit over the app's lifetime climbed the schedule and a tenth restart
    /// waited the full 30 s (seen in the field on 8 September 2026).
    static let healthyUptime: TimeInterval = 60

    static func nextAttempt(after uptime: TimeInterval, previous: Int) -> Int {
        uptime >= healthyUptime ? 0 : min(previous + 1, delays.count - 1)
    }

    private var attempt = 0
    private var startedAt = Date()

    /// The delay the current attempt waits before retrying.
    var delay: Duration { .seconds(Self.delays[attempt]) }

    /// Marks the start of a run whose uptime decides the next delay.
    mutating func runStarted() {
        startedAt = Date()
    }

    /// After a run ends: resets the schedule if the run was healthy, otherwise
    /// climbs it, then sleeps the resulting delay.
    mutating func next() async {
        attempt = Self.nextAttempt(after: Date().timeIntervalSince(startedAt), previous: attempt)
        await wait()
    }

    /// Climbs the schedule after a failure that has no run uptime.
    mutating func recordFailure() {
        attempt = min(attempt + 1, Self.delays.count - 1)
    }

    mutating func reset() {
        attempt = 0
    }

    /// Sleeps the current delay without moving along the schedule.
    func wait() async {
        try? await Task.sleep(for: delay)
    }
}
