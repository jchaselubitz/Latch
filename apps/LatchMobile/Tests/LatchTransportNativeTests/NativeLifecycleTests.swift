import Foundation
import XCTest
import LatchTransportNative

final class NativeLifecycleTests: XCTestCase {
    func testGeneratedBoundaryClosesPendingConnectAndFiltersTheTrace() async throws {
        let log = FileManager.default.temporaryDirectory
            .appendingPathComponent("latch-native-test-\(UUID().uuidString).log")
        try configureIceDiagnostics(path: log.path)
        defer {
            try? configureIceDiagnostics(path: nil)
            try? FileManager.default.removeItem(at: log)
        }
        let transport = try await RemoteTransport.gather(
            credentials: IceCredentials(ufrag: "swift-local-test", password: "local-password-with-over-128-bits"),
            servers: []
        )
        let pending = Task {
            try await transport.connect(
                remote: RemoteDescription(
                    credentials: IceCredentials(ufrag: "swift-missing-peer", password: "NEVER-LOG-THIS-REMOTE-PASSWORD"),
                    candidates: []
                ), role: .initiator
            )
        }
        // Wait for the native connect start marker, not a guessed scheduling delay.
        let deadline = Date().addingTimeInterval(3)
        while !(try String(contentsOf: log, encoding: .utf8)).contains("connect role=Initiator"),
              Date() < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        let started = try String(contentsOf: log, encoding: .utf8)
        XCTAssertTrue(started.contains("connect role=Initiator"))
        let beforeClose = Date()
        try await transport.close()
        do {
            _ = try await pending.value
            XCTFail("closed connection unexpectedly succeeded")
        } catch TransportError.InvalidState {}
        XCTAssertLessThan(Date().timeIntervalSince(beforeClose), 3,
                          "close waited for the fifteen-second ICE deadline")
        XCTAssertFalse(transport.connectivityFailed())
        try await transport.close()
        let contents = try String(contentsOf: log, encoding: .utf8)
        XCTAssertTrue(contents.contains("lifecycle-v2"))
        XCTAssertTrue(contents.contains("transport cancelled"))
        XCTAssertFalse(contents.contains("NEVER-LOG-THIS-REMOTE-PASSWORD"))
        XCTAssertFalse(contents.contains("remotePwd"))
    }
}
