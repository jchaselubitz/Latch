import XCTest
@testable import LatchDesktop

@MainActor
final class ModelTests: XCTestCase {
    private func makeCLI(scriptBody: String) throws -> (directory: URL, executable: URL) {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("latch-client-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let executable = directory.appendingPathComponent("latch")
        let script = "#!/bin/sh\n\(scriptBody)\n"
        try Data(script.utf8).write(to: executable)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755],
            ofItemAtPath: executable.path
        )
        return (directory, executable)
    }

    private func makeCLI(response: String) throws -> (directory: URL, executable: URL) {
        let fixture = try makeCLI(scriptBody: "")
        let responseURL = fixture.directory.appendingPathComponent("response.json")
        try Data(response.utf8).write(to: responseURL)
        let script = "#!/bin/sh\nexec /bin/cat \(TerminalLauncher.shellQuote(responseURL.path))\n"
        try Data(script.utf8).write(to: fixture.executable)
        return fixture
    }

    func testWhereOutputKeepsExecutablePathsInOrderAndRemovesDuplicates() {
        let output = "/bin/sh\n/bin/sh\nlatch: aliased to something\n/usr/bin/false\n"
        XCTAssertEqual(
            LatchClient.executablePaths(in: output),
            ["/bin/sh", "/usr/bin/false"]
        )
    }

    func testListDecodesEverySessionStateAndUnknownFields() throws {
        let states = ["running", "exited", "lost"]
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

    func testCurrentManagementResponsesDecodeAllContractFields() throws {
        let decoder = JSONDecoder()
        let stop = try decoder.decode(
            StopReport.self,
            from: Data(#"{"id":"ses_1","state":"exited","stopped":true}"#.utf8)
        )
        XCTAssertTrue(stop.stopped)

        let resize = try decoder.decode(
            ResizeReport.self,
            from: Data(#"{"id":"ses_1","cols":132,"rows":44,"pinned":true}"#.utf8)
        )
        XCTAssertEqual(resize.cols, 132)
        XCTAssertEqual(resize.rows, 44)
        XCTAssertTrue(resize.pinned)

        let doctor = try decoder.decode(
            DoctorReport.self,
            from: Data(#"{"tmuxVersion":"tmux 3.7b","findings":[]}"#.utf8)
        )
        XCTAssertEqual(doctor.tmuxVersion, "tmux 3.7b")
    }

    func testCapabilitiesDecodeCurrentFeatureMetadata() throws {
        let report = try JSONDecoder().decode(
            CapabilitiesReport.self,
            from: Data("""
            {"protocolVersion":1,"productVersion":"0.2608132217.0","capabilities":{
              "create":true,"openViewer":true,"localAttach":true,"cloudAttach":false,
              "selfUpdate":true,"extensions":["harness-events-v1","harness-interaction-v1"]}}
            """.utf8)
        )
        XCTAssertTrue(report.capabilities.selfUpdate)
        XCTAssertEqual(
            report.capabilities.extensions,
            ["harness-events-v1", "harness-interaction-v1"]
        )
    }

    func testCLIUpdateResponseUsesTheRustWireNames() throws {
        let report = try JSONDecoder().decode(
            CLIUpdateReport.self,
            from: Data("""
            {"status":"available","current_version":"0.1.0","latest_version":"0.2.0",
             "release_url":"https://example.invalid/release"}
            """.utf8)
        )
        XCTAssertEqual(report.status, .available)
        XCTAssertEqual(report.currentVersion, "0.1.0")
        XCTAssertEqual(report.latestVersion, "0.2.0")
        XCTAssertEqual(report.releaseURL?.absoluteString, "https://example.invalid/release")
        XCTAssertNil(report.installedPath)
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

    func testClientDrainsAResponseLargerThanTheProcessPipeBuffer() async throws {
        let padding = String(repeating: "x", count: 256_000)
        let fixture = try makeCLI(response: #"{"sessions":[],"future":"\#(padding)"}"#)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }

        let client = LatchClient(executableURL: fixture.executable, timeout: 2)
        let report = try await client.list()
        XCTAssertTrue(report.sessions.isEmpty)
    }

    func testClientRejectsThePreTmuxDesktopContract() async throws {
        let fixture = try makeCLI(response: """
        {"protocolVersion":1,"productVersion":"0.2608131641.0","capabilities":{
          "create":true,"openViewer":true,"localAttach":true,"cloudAttach":false,
          "selfUpdate":true,"extensions":[]}}
        """)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }

        let client = LatchClient(executableURL: fixture.executable)
        do {
            _ = try await client.validateCompatibility()
            XCTFail("expected an incompatible product error")
        } catch let error as LatchClientError {
            XCTAssertEqual(
                error,
                .incompatibleProduct(
                    minimum: LatchClient.minimumProductVersion,
                    actual: "0.2608131641.0"
                )
            )
        }
    }

    func testClientTerminatesATimedOutManagementCommand() async throws {
        let fixture = try makeCLI(scriptBody: "exec /bin/sleep 5")
        defer { try? FileManager.default.removeItem(at: fixture.directory) }

        let client = LatchClient(executableURL: fixture.executable, timeout: 0.05)
        do {
            let _: ListReport = try await client.list()
            XCTFail("expected a timeout")
        } catch let error as LatchClientError {
            XCTAssertEqual(error, .timeout)
        }
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
