import Foundation
import XCTest

@testable import LatchMobileKit

/// The phone's own path counters: what a field run reads afterwards.
@MainActor
final class RemotePathMetricsTests: XCTestCase {
    private let controlPlane = URL(string: "https://control.example")!

    func testEveryOpenedChannelIsCountedEvenWhenThePathDoesNotChange() {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())

        reporter.report(.relay)
        reporter.report(.relay)
        reporter.report(.relay)

        // The indicator saw one transition; the network relayed three times.
        // Deduplicating the count would make a relay-only network read as a
        // single blip.
        XCTAssertEqual(reporter.tally.relay, 3)
        XCTAssertEqual(reporter.tally.connections, 3)
        XCTAssertEqual(reporter.path, .relay)
    }

    func testClearingTheIndicatorDoesNotDisturbTheCounters() {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())

        reporter.report(.direct)
        reporter.clear()

        XCTAssertNil(reporter.path)
        XCTAssertEqual(reporter.tally.direct, 1)
        XCTAssertEqual(reporter.tally.connections, 1)
    }

    func testRelayShareIsUnmeasuredRatherThanZeroBeforeAnyConnection() {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())

        XCTAssertNil(reporter.tally.relayShare)
        XCTAssertNil(reporter.tally.summary)

        reporter.reportFailure()

        // A failed attempt is an attempt, but it is not a connection: it can
        // move the summary without inventing a relay share.
        XCTAssertEqual(reporter.tally.attempts, 1)
        XCTAssertEqual(reporter.tally.connections, 0)
        XCTAssertNil(reporter.tally.relayShare)
        XCTAssertEqual(reporter.tally.summary, "Failed 1")
    }

    func testRelayShareCountsOnlyOpenedChannels() {
        var tally = RemotePathTally()
        tally.record(.direct)
        tally.record(.direct)
        tally.record(.direct)
        tally.record(.relay)
        tally.failures += 5

        XCTAssertEqual(tally.relayShare, 0.25)
        XCTAssertEqual(tally.summary, "Direct 3 · Relay 1 · Failed 5")
    }

    func testACountedTallySurvivesANewReporterOnTheSameStore() {
        let store = EphemeralRemotePathMetricsStore()
        RemotePathReporter(metrics: store).report(.local)

        XCTAssertEqual(RemotePathReporter(metrics: store).tally.local, 1)
    }

    func testResettingClearsTheCountersAndNotifiesTheObserver() {
        let reporter = RemotePathReporter(metrics: EphemeralRemotePathMetricsStore())
        reporter.report(.relay)
        let observed = TallyBox()
        reporter.observeTally { observed.value = $0 }

        reporter.resetTally()

        XCTAssertEqual(observed.value?.connections, 0)
        XCTAssertEqual(reporter.tally, RemotePathTally())
    }

}

/// A reference box so a `@Sendable` observer can hand its value back out.
private final class TallyBox: @unchecked Sendable {
    var value: RemotePathTally?
}
