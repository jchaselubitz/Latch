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
}
