import XCTest
@testable import LatchDesktop

final class GeminiSettingsTests: XCTestCase {
    func testSelectedCLIReceivesKeyOnStdinAndReportsStatus() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let executable = directory.appendingPathComponent("latch")
        let script = """
        #!/bin/sh
        case "$*" in
          'commit-message set-key --stdin') read key; [ "$key" = 'fake_key_123456789' ] ;;
          'commit-message status') printf 'ready (local configuration only; provider access not verified)\\n' ;;
          'commit-message remove-key') exit 0 ;;
          *) exit 1 ;;
        esac
        """
        try script.write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: executable.path)
        let client = LatchClient(executableURL: executable)
        try await client.setGeminiKey("fake_key_123456789")
        let status = try await client.geminiKeyStatus()
        XCTAssertTrue(status.hasPrefix("ready"))
        try await client.removeGeminiKey()
    }
}
