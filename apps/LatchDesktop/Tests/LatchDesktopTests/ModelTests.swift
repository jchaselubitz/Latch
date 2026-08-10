import XCTest
@testable import LatchDesktop

final class ModelTests: XCTestCase {
    func testListDecodesEverySessionStateAndUnknownFields() throws {
        let states = ["creating", "running", "stopping", "exited", "lost"]
        let sessions = states.enumerated().map { index, state in
            """
            {"id":"ses_\(index)","name":"demo","state":"\(state)","cwd":"/tmp",\
            "command_label":"zsh","created_at":"2026-08-10T00:00:00Z","future":true}
            """
        }.joined(separator: ",")
        let report = try JSONDecoder().decode(
            ListReport.self,
            from: Data("{\"sessions\":[\(sessions)]}".utf8)
        )
        XCTAssertEqual(report.sessions.map(\.state), SessionState.allCases)
    }

    func testMissingRequiredStateIsRejected() {
        let data = Data("""
        {"sessions":[{"id":"ses_1","name":"demo","cwd":"/tmp",\
        "command_label":"zsh","created_at":"2026-08-10T00:00:00Z"}]}
        """.utf8)
        XCTAssertThrowsError(try JSONDecoder().decode(ListReport.self, from: data))
    }

    func testRemoveResponseDecodes() throws {
        let report = try JSONDecoder().decode(
            RemoveReport.self,
            from: Data(#"{"id":"ses_1","removed":true}"#.utf8)
        )
        XCTAssertTrue(report.removed)
    }

    func testCreateManifestUsesTheRustWireNames() throws {
        let manifest = LaunchManifest(
            launch: .init(
                argv: ["/bin/zsh", "-l"],
                cwd: "/tmp",
                size: .init(cols: 120, rows: 36)
            ),
            display: .init(name: "demo", title: nil, commandLabel: "zsh")
        )
        let value = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(manifest)) as? [String: Any]
        )
        XCTAssertEqual(value["format_version"] as? Int, 1)
        let launch = try XCTUnwrap(value["launch"] as? [String: Any])
        XCTAssertEqual(launch["inherit_env"] as? Bool, true)
        let display = try XCTUnwrap(value["display"] as? [String: Any])
        XCTAssertEqual(display["command_label"] as? String, "zsh")
        XCTAssertEqual((display["source"] as? [String: Any])?["kind"] as? String, "desktop")
    }
}

@MainActor
final class TerminalLauncherTests: XCTestCase {
    func testShellQuotePreservesSpacesAndApostrophes() {
        XCTAssertEqual(
            TerminalLauncher.shellQuote("/Applications/Latch's App/latch"),
            "'/Applications/Latch'\"'\"'s App/latch'"
        )
    }

    func testAppleScriptEscapeDoesNotCreateSourceLines() {
        XCTAssertEqual(TerminalLauncher.appleScriptEscape("a\n\"b\\c"), "a\\n\\\"b\\\\c")
    }

    func testCustomTemplateParsingDoesNotInvokeShellSyntax() throws {
        XCTAssertEqual(
            try TerminalLauncher.parseArguments(#"--new-tab -e "{latch}" attach '{session}'"#),
            ["--new-tab", "-e", "{latch}", "attach", "{session}"]
        )
        XCTAssertEqual(try TerminalLauncher.parseArguments("'a; rm -rf nope'"), ["a; rm -rf nope"])
    }

    func testCustomTemplateRejectsUnmatchedQuotes() {
        XCTAssertThrowsError(try TerminalLauncher.parseArguments("'unfinished"))
    }

    func testCustomTerminalSelectionRejectsNonApplicationPaths() {
        XCTAssertThrowsError(
            try TerminalLauncher.executablePath(forApplicationURL: URL(fileURLWithPath: "/tmp/not-an-app"))
        )
    }
}
