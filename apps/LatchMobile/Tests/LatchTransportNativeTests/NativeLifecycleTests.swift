import Foundation
import LatchTransportNative
import XCTest

final class NativeLifecycleTests: XCTestCase {
    func testGeneratedRemoteLinkBoundaryRejectsInvalidLANTarget() async {
        do {
            _ = try await RemoteLink.connectLan(
                host: "",
                port: 0,
                purpose: .session,
                role: .controller,
                localPrivateKey: Data(),
                localPublicKey: Data(),
                expectedRemotePublicKey: nil,
                enrollmentId: nil,
                enrollmentSecret: nil,
                grantRevision: 1
            )
            XCTFail("invalid LAN target unexpectedly succeeded")
        } catch let TransportError.Failure(message) {
            XCTAssertEqual(message, "invalid LAN target")
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testGeneratedRemoteLinkBoundaryRejectsCleartextRelayURL() async {
        do {
            _ = try await RemoteLink.connectWss(
                url: "ws://relay.invalid/v1/connect",
                admission: "admission",
                purpose: .session,
                role: .controller,
                localPrivateKey: Data(),
                localPublicKey: Data(),
                expectedRemotePublicKey: nil,
                enrollmentId: nil,
                enrollmentSecret: nil,
                grantRevision: 1,
                peerWaitMs: 1
            )
            XCTFail("cleartext relay URL unexpectedly succeeded")
        } catch let TransportError.Failure(message) {
            XCTAssertEqual(message, "invalid remote-link configuration: relay url must use wss://")
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}
