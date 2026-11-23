import CryptoKit
import Foundation
import XCTest
@testable import LatchNotificationKit

final class AttentionPayloadTests: XCTestCase {
    /// The vector in `crates/latch/src/cli/serve/attention/seal.rs`
    /// (`interop`): sealed by the Rust `hpke` crate on the Mac side.
    let secret = "896615161d5c4dab6be99bb69b180f22c86eb0ac1b8912e958ed5dd08813573b"
    let event = "0123456789abcdef0123456789abcdef"
    let wire = "5395375bbdd01f8bf21bdef04680cbec79f3a2f8b3c110e5d0b3186636666c448553f9cf22c79b1b6ab25b15835e5f5256bacc499ec9f868201ccbdceea3c8f61a11422f256646f245c9acb01acb68787147a91ac93d9058e19acb2071a7acdbf93308d752a83beea3bea4abd7f8d548e758c16f473d3176e61a7541985ac6"

    private var key: Curve25519.KeyAgreement.PrivateKey {
        try! Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data(hex: secret)!)
    }

    func testRichRequestNotificationUsesSealedClassification() throws {
        let raw = Data(#"{"v":1,"s":"session","n":"Agent session","k":"approval","r":"app_access","a":"Safari"}"#.utf8)
        let payload = try JSONDecoder().decode(AttentionPayload.self, from: raw)
        XCTAssertEqual(AttentionNotification(payload).body, "Wants to use Safari")
        let plan = AttentionPayload(sessionID: "session", name: "Agent session", title: nil,
                                    kind: .approval, requestKind: "plan_approval")
        XCTAssertEqual(AttentionNotification(plan).body, "Plan ready for approval")
    }

    func testCodexRequestKindsUseSpecificNotificationCopy() {
        for (kind, expected) in [
            ("browser", "Browser access requested"),
            ("permissions", "Additional permissions requested"),
            ("question", "Has a question"),
            ("elicitation_form", "Information requested"),
            ("elicitation_url", "Action requested")
        ] {
            let payload = AttentionPayload(sessionID: "session", name: "Codex", title: nil,
                                           kind: .approval, requestKind: kind)
            XCTAssertEqual(AttentionNotification(payload).body, expected)
        }
    }

    func testOpensWhatTheMacSealed() throws {
        let payload = try AttentionPayload.open(sealedHex: wire, eventID: event, privateKey: key)
        XCTAssertEqual(payload, AttentionPayload(sessionID: "ses_vector", name: "auth-refactor", title: "Refactor auth", kind: .approval))
    }

    func testRefusesAnotherEventAnotherKeyOrTampering() {
        XCTAssertThrowsError(try AttentionPayload.open(sealedHex: wire, eventID: String(repeating: "f", count: 32), privateKey: key))
        XCTAssertThrowsError(try AttentionPayload.open(sealedHex: wire, eventID: event, privateKey: .init()))
        var tampered = Array(wire)
        tampered[tampered.count - 1] = tampered.last == "0" ? "1" : "0"
        XCTAssertThrowsError(try AttentionPayload.open(sealedHex: String(tampered), eventID: event, privateKey: key))
        XCTAssertThrowsError(try AttentionPayload.open(sealedHex: "zz", eventID: event, privateKey: key))
    }

    func testWordsAndRoutingComeFromThePayload() {
        let approval = AttentionNotification(AttentionPayload(sessionID: "ses_1", name: "auth", title: "  ", kind: .approval))
        XCTAssertEqual(approval.title, "auth", "a blank title falls back to the name")
        XCTAssertEqual(approval.body, "Needs your input")
        XCTAssertEqual(approval.threadIdentifier, "latch-session-ses_1")
        let done = AttentionNotification(AttentionPayload(sessionID: "ses_2", name: "n", title: "Refactor auth", kind: .completion))
        XCTAssertEqual(done.title, "Refactor auth")
        XCTAssertEqual(done.body, "Finished working")
        let background = AttentionNotification(AttentionPayload(sessionID: "ses_3", name: "n", title: nil, kind: .completion, backgroundTasks: 2))
        XCTAssertEqual(background.body, "Turn finished · 2 background tasks still running")
        let one = AttentionNotification(AttentionPayload(sessionID: "ses_3", name: "n", title: nil, kind: .completion, backgroundTasks: 1))
        XCTAssertEqual(one.body, "Turn finished · 1 background task still running")
    }

    func testDecodesTheBackgroundCountAndToleratesItsAbsence() throws {
        let json = Data(#"{"v":1,"s":"ses_1","n":"n","k":"completion","b":3}"#.utf8)
        XCTAssertEqual(try JSONDecoder().decode(AttentionPayload.self, from: json).backgroundTasks, 3)
        let plain = Data(#"{"v":1,"s":"ses_1","n":"n","k":"completion"}"#.utf8)
        XCTAssertNil(try JSONDecoder().decode(AttentionPayload.self, from: plain).backgroundTasks)
    }

    func testFindsSealedFieldsOnlyInTheExpectedShape() {
        let userInfo: [AnyHashable: Any] = ["aps": [:], "latch": ["e": event, "c": wire]]
        XCTAssertEqual(AttentionNotification.sealedFields(in: userInfo)?.eventID, event)
        XCTAssertNil(AttentionNotification.sealedFields(in: ["latch": ["e": "nothex", "c": wire]]))
        XCTAssertNil(AttentionNotification.sealedFields(in: ["aps": [:]]))
        XCTAssertEqual(AttentionNotification.sessionID(in: [AttentionNotification.sessionIDKey: "ses_1"]), "ses_1")
    }

    func testKeyIsCreatedOnceAndReused() {
        final class Box: @unchecked Sendable { var data: Data? }
        let box = Box()
        let store = NotificationKeyStore(storage: .init(read: { box.data }, write: { box.data = $0; return true }))
        XCTAssertNil(store.load())
        let first = store.loadOrCreate()
        XCTAssertNotNil(first)
        XCTAssertEqual(store.loadOrCreate()?.rawRepresentation, first?.rawRepresentation)
        XCTAssertEqual(NotificationKeyStore.publicKeyHex(first!).count, 64)

        let refusing = NotificationKeyStore(storage: .init(read: { nil }, write: { _ in false }))
        XCTAssertNil(refusing.loadOrCreate(), "a key that could not be stored must not be registered")
    }
}
