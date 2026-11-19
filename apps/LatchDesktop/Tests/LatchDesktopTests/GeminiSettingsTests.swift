import XCTest
@testable import LatchDesktop

final class GeminiSettingsTests: XCTestCase {
    func testKeyValidationAcceptsPeriodsAndRejectsUnsafeInput() {
        XCTAssertTrue(RepositorySettingsView.isValidKey("AQ.fixture_key-123456789"))
        XCTAssertTrue(RepositorySettingsView.isValidKey("sk-or-v1-0123456789abcdef0123456789abcdef"))
        for key in ["short", "abcdefghijklmnop\n", "abcdefghijklmnop:header", String(repeating: "a", count: 513)] {
            XCTAssertFalse(RepositorySettingsView.isValidKey(key))
        }
    }

    func testModelAndRoutingValidationFollowsProvider() {
        XCTAssertTrue(RepositorySettingsView.isValidModel("gemini-3.5-flash-lite", provider: .gemini))
        XCTAssertFalse(RepositorySettingsView.isValidModel("google/gemini-3.5-flash-lite", provider: .gemini))
        for model in ["google/gemini-3.5-flash-lite", "openai/gpt-5:free", "~anthropic/claude-sonnet-latest"] {
            XCTAssertTrue(RepositorySettingsView.isValidModel(model, provider: .openrouter), model)
        }
        for model in ["gemini-3.5-flash-lite", "vendor/", "a//b", "vendor/../x", "vendor/model name"] {
            XCTAssertFalse(RepositorySettingsView.isValidModel(model, provider: .openrouter), model)
        }
        XCTAssertEqual(RepositorySettingsView.parseRouting(" google-vertex, ,azure "), ["google-vertex", "azure"])
        XCTAssertTrue(RepositorySettingsView.isValidRouting([]))
        XCTAssertFalse(RepositorySettingsView.isValidRouting(["bad slug"]))
        XCTAssertFalse(RepositorySettingsView.isValidRouting(Array(repeating: "a", count: 9)))
    }

    func testSelectedCLIReceivesProviderKeyOnStdinAndReportsStructuredStatus() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let executable = directory.appendingPathComponent("latch")
        let script = """
        #!/bin/sh
        case "$*" in
          'commit-message set-key --stdin --provider openrouter') read key; [ "$key" = 'fake_key_123456789' ] ;;
          'commit-message status --json') printf '%s\\n' '{"state":"ready","provider":"openrouter","providers":{"gemini":{"key":"missing","model":"gemini-3.5-flash-lite","defaultModel":"gemini-3.5-flash-lite","customModel":false},"openrouter":{"key":"present","model":"openai/gpt-5","defaultModel":"google/gemini-3.5-flash-lite","customModel":true}},"openrouterRouting":{"order":["azure"],"allowFallbacks":false}}' ;;
          'commit-message model --provider openrouter -- openai/gpt-5') exit 0 ;;
          'commit-message model --provider openrouter --reset') exit 0 ;;
          'commit-message provider openrouter') exit 0 ;;
          'commit-message routing --order azure,google-vertex --allow-fallbacks false') exit 0 ;;
          'commit-message remove-key --provider openrouter') exit 0 ;;
          *) exit 1 ;;
        esac
        """
        try script.write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: executable.path)
        let client = LatchClient(executableURL: executable)
        try await client.setCommitMessageKey("fake_key_123456789", provider: .openrouter)
        try await client.setCommitMessageProvider(.openrouter)
        try await client.setCommitMessageModel("openai/gpt-5", provider: .openrouter)
        try await client.setCommitMessageModel(nil, provider: .openrouter)
        try await client.setOpenRouterRouting(order: ["azure", "google-vertex"], allowFallbacks: false)
        let status = try await client.commitMessageStatus()
        XCTAssertEqual(status.state, "ready")
        XCTAssertEqual(status.provider, .openrouter)
        XCTAssertEqual(status.state(for: .openrouter)?.model, "openai/gpt-5")
        XCTAssertEqual(status.state(for: .openrouter)?.customModel, true)
        XCTAssertEqual(status.openrouterRouting, .init(order: ["azure"], allowFallbacks: false))
        try await client.removeCommitMessageKey(provider: .openrouter)
    }
}
