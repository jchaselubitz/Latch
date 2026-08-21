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

        let stopAll = try decoder.decode(
            StopAllReport.self,
            from: Data(#"{"sessions":[{"id":"ses_1","state":"exited","stopped":true}]}"#.utf8)
        )
        XCTAssertEqual(stopAll.sessions.count, 1)

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
            {"protocolVersion":2,"productVersion":"0.2608161432.0","capabilities":{
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

    func testClientAcceptsProtocolTwo() async throws {
        let fixture = try makeCLI(response: """
        {"protocolVersion":2,"productVersion":"0.2608161432.0","capabilities":{
          "create":true,"openViewer":true,"localAttach":true,"cloudAttach":false,
          "selfUpdate":true,"extensions":[]}}
        """)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }

        let report = try await LatchClient(executableURL: fixture.executable).validateCompatibility()
        XCTAssertEqual(report.protocolVersion, LatchClient.supportedProtocolVersion)
    }

    func testClientRejectsProtocolOne() async throws {
        let fixture = try makeCLI(response: """
        {"protocolVersion":1,"productVersion":"0.2608161432.0","capabilities":{
          "create":true,"openViewer":true,"localAttach":true,"cloudAttach":false,
          "selfUpdate":true,"extensions":[]}}
        """)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }

        let client = LatchClient(executableURL: fixture.executable)
        do {
            _ = try await client.validateCompatibility()
            XCTFail("expected an incompatible protocol error")
        } catch let error as LatchClientError {
            XCTAssertEqual(
                error,
                .incompatibleProtocol(
                    expected: LatchClient.supportedProtocolVersion,
                    actual: 1,
                    productVersion: "0.2608161432.0"
                )
            )
        }
    }

    func testClientRejectsAReleaseBeforeStopAllSupport() async throws {
        let fixture = try makeCLI(response: """
        {"protocolVersion":2,"productVersion":"0.2608161004.0","capabilities":{
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
                    actual: "0.2608161004.0"
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
            XCTAssertEqual(error, .timeout(operation: "list", seconds: 0.05))
            XCTAssertEqual(
                error.errorDescription,
                "`latch list` did not respond within 1s and was cancelled. Retry, or run Doctor if it keeps happening."
            )
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

    func testScriptableTerminalsOfferEveryOpenBehavior() {
        for terminal in [PreferredTerminal.terminal, .iTerm] {
            XCTAssertEqual(terminal.supportedOpenBehaviors, TerminalOpenBehavior.allCases)
            for behavior in TerminalOpenBehavior.allCases {
                XCTAssertEqual(terminal.resolvedOpenBehavior(behavior), behavior)
                XCTAssertNil(terminal.unsupportedReason(for: behavior))
            }
        }
    }

    func testUnsupportedBehaviorsFallBackToANewWindowWithAReason() {
        XCTAssertEqual(PreferredTerminal.ghostty.supportedOpenBehaviors, [.newWindow])
        XCTAssertEqual(PreferredTerminal.ghostty.resolvedOpenBehavior(.newTab), .newWindow)
        XCTAssertNotNil(PreferredTerminal.ghostty.unsupportedReason(for: .newTab))
        XCTAssertNil(PreferredTerminal.ghostty.unsupportedReason(for: .newWindow))

        // A custom terminal is launched entirely by its argument template.
        XCTAssertTrue(PreferredTerminal.custom.supportedOpenBehaviors.isEmpty)
        for behavior in TerminalOpenBehavior.allCases {
            XCTAssertEqual(PreferredTerminal.custom.resolvedOpenBehavior(behavior), .newWindow)
            XCTAssertNotNil(PreferredTerminal.custom.unsupportedReason(for: behavior))
        }
    }

    func testOpenRejectsMalformedAttachmentCommandsForEveryBehavior() {
        for behavior in TerminalOpenBehavior.allCases {
            for openInBackground in [false, true] {
                XCTAssertThrowsError(
                    try TerminalLauncher.open(
                        command: ["latch", "attach"],
                        in: .terminal,
                        behavior: behavior,
                        openInBackground: openInBackground
                    )
                ) { error in
                    XCTAssertEqual(error as? TerminalLaunchError, .invalidCommand)
                }
            }
        }
    }

    func testStoredOpenBehaviorSurvivesAnUnsupportedTerminalChoice() {
        let defaults = UserDefaults.standard
        let previousTerminal = defaults.string(forKey: "preferredTerminal")
        let previousBehavior = defaults.string(forKey: "terminalOpenBehavior")
        defer {
            defaults.set(previousTerminal, forKey: "preferredTerminal")
            defaults.set(previousBehavior, forKey: "terminalOpenBehavior")
        }

        let store = SessionStore()
        store.preferredTerminal = .iTerm
        store.terminalOpenBehavior = .newTab
        XCTAssertEqual(store.effectiveOpenBehavior, .newTab)

        store.preferredTerminal = .ghostty
        XCTAssertEqual(store.terminalOpenBehavior, .newTab)
        XCTAssertEqual(store.effectiveOpenBehavior, .newWindow)
    }

    func testStoredBackgroundLaunchPreferenceRoundTrips() {
        let defaults = UserDefaults.standard
        let previous = defaults.object(forKey: "terminalOpenInBackground")
        defer {
            if let previous = previous as? Bool {
                defaults.set(previous, forKey: "terminalOpenInBackground")
            } else {
                defaults.removeObject(forKey: "terminalOpenInBackground")
            }
        }

        defaults.removeObject(forKey: "terminalOpenInBackground")
        let store = SessionStore()
        XCTAssertFalse(store.terminalOpenInBackground)

        store.terminalOpenInBackground = true
        XCTAssertTrue(defaults.bool(forKey: "terminalOpenInBackground"))
        XCTAssertTrue(SessionStore().terminalOpenInBackground)
    }

    func testBackgroundLaunchOmitsActivateFromScriptableTerminals() {
        let command = "latch attach ses_1"
        let foreground = TerminalLauncher.applicationScript(
            application: "Terminal",
            statement: "do script",
            shellCommand: command,
            activates: true
        )
        let background = TerminalLauncher.applicationScript(
            application: "Terminal",
            statement: "do script",
            shellCommand: command,
            activates: false
        )
        XCTAssertTrue(foreground.contains("\n    activate\n"))
        XCTAssertFalse(background.contains("activate"))
        XCTAssertTrue(background.contains("do script"))

        let iTermForeground = TerminalLauncher.iTermTabScript(shellCommand: command, activates: true)
        let iTermBackground = TerminalLauncher.iTermTabScript(shellCommand: command, activates: false)
        XCTAssertTrue(iTermForeground.contains("\n    activate\n"))
        XCTAssertFalse(iTermBackground.contains("activate"))
        XCTAssertTrue(iTermBackground.contains("create tab with default profile command"))
    }

    func testDisplayIdleLabelMatchesSidebarBuckets() {
        XCTAssertEqual(SessionSummary.displayIdleLabel(for: 12_000), "12s idle")
        XCTAssertEqual(SessionSummary.displayIdleLabel(for: 180_000), "3m idle")
        XCTAssertEqual(SessionSummary.displayIdleLabel(for: 7_200_000), "2h idle")
        XCTAssertEqual(SessionSummary.displayIdleLabel(for: 172_800_000), "2d idle")
        XCTAssertNil(SessionSummary.displayIdleLabel(for: nil))
    }

    func testSessionDisplayEqualityIgnoresRawIdleChurnInsideOneBucket() throws {
        let base = try JSONDecoder().decode(
            SessionSummary.self,
            from: Data("""
            {"id":"ses_1","name":"demo","state":"running","cwd":"/tmp",\
            "command_label":"zsh","created_at":"2026-08-10T00:00:00Z",\
            "last_activity_at":"2026-08-10T00:03:00Z","idle_ms":180000}
            """.utf8)
        )
        let sameMinute = try JSONDecoder().decode(
            SessionSummary.self,
            from: Data("""
            {"id":"ses_1","name":"demo","state":"running","cwd":"/tmp",\
            "command_label":"zsh","created_at":"2026-08-10T00:00:00Z",\
            "last_activity_at":"2026-08-10T00:03:20Z","idle_ms":200000}
            """.utf8)
        )
        let nextMinute = try JSONDecoder().decode(
            SessionSummary.self,
            from: Data("""
            {"id":"ses_1","name":"demo","state":"running","cwd":"/tmp",\
            "command_label":"zsh","created_at":"2026-08-10T00:00:00Z",\
            "last_activity_at":"2026-08-10T00:04:00Z","idle_ms":240000}
            """.utf8)
        )

        XCTAssertTrue(base.isDisplayEqual(to: sameMinute))
        XCTAssertFalse(base.isDisplayEqual(to: nextMinute))
        XCTAssertNotEqual(base, sameMinute)
    }

    func testAppChromeStateOnlyPublishesWhenValuesChange() {
        let chrome = AppChromeState()
        var runningUpdates = 0
        var createUpdates = 0
        let running = chrome.$runningCount.sink { _ in runningUpdates += 1 }
        let create = chrome.$canCreateSessions.sink { _ in createUpdates += 1 }
        defer {
            _ = running
            _ = create
        }

        chrome.update(runningCount: 0, canCreateSessions: false)
        chrome.update(runningCount: 2, canCreateSessions: true)
        chrome.update(runningCount: 2, canCreateSessions: true)

        XCTAssertEqual(runningUpdates, 2)
        XCTAssertEqual(createUpdates, 2)
        XCTAssertEqual(chrome.runningCount, 2)
        XCTAssertTrue(chrome.canCreateSessions)
    }
}
