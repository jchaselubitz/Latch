import XCTest

@testable import LatchMobileKit

/// The session list is the only source a tap can be routed from without
/// opening a socket, and routing turns on three answers, not two: a gateway
/// that never heard of the field, a session that has no connector, and a
/// session that has one. `decodeIfPresent` folds the first two together, so
/// the decoder is written out and these tests hold it to all three.
final class SessionConnectorTests: XCTestCase {
    private func decode(_ connectorField: String) throws -> SessionSummary {
        let json = """
        {"id":"ses_1","name":"latch","state":"running","cwd":"/Users/jake/Latch",
         "command_label":"claude","created_at":"2026-08-24T09:00:00Z"\(connectorField)}
        """
        return try JSONDecoder().decode(SessionSummary.self, from: Data(json.utf8))
    }

    func testAnOmittedFieldIsNotAClaimThatThereIsNoConnector() throws {
        XCTAssertEqual(try decode("").connector, .unknown)
    }

    func testAnExplicitNullMeansTheSessionHasNoConnector() throws {
        XCTAssertEqual(try decode(#","connector":null"#).connector, .none)
    }

    func testANamedConnectorDecodesToItsName() throws {
        let summary = try decode(#","connector":"claude""#)
        XCTAssertEqual(summary.connector, .named("claude"))
        XCTAssertEqual(summary.connector.name, "claude")
    }

    func testTheOtherFieldsStillDecode() throws {
        let summary = try decode(
            #","connector":null,"last_activity_at":"2026-08-24T09:01:00Z","idle_ms":1200,"title":"Latch""#
        )
        XCTAssertEqual(summary.id, "ses_1")
        XCTAssertEqual(summary.commandLabel, "claude")
        XCTAssertEqual(summary.lastActivityAt, "2026-08-24T09:01:00Z")
        XCTAssertEqual(summary.idleMs, 1200)
        XCTAssertEqual(summary.displayName, "Latch")
        XCTAssertEqual(summary.directoryName, "Latch")
        XCTAssertTrue(summary.isRunning)
    }

    func testTheListReportCarriesTheFieldThrough() throws {
        let json = """
        {"sessions":[
          {"id":"ses_shell","name":"shell","state":"running","cwd":"/tmp",
           "command_label":"zsh","created_at":"2026-08-24T09:00:00Z","connector":null},
          {"id":"ses_agent","name":"agent","state":"running","cwd":"/tmp",
           "command_label":"claude","created_at":"2026-08-24T09:00:00Z","connector":"claude"}
        ]}
        """
        let report = try JSONDecoder().decode(ListReport.self, from: Data(json.utf8))
        XCTAssertEqual(report.sessions.map(\.connector), [.none, .named("claude")])
    }
}
