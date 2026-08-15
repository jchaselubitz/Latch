import XCTest

@testable import LatchMobileKit

/// The generated types must decode what `latch serve` actually sends, and must
/// keep working when a newer gateway sends more than this build knows about.
final class ContractTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testDecodesTheDiscoveryDocument() throws {
        // Shaped like the real response, including the engine capabilities the
        // gateway flattens in alongside the documented fields.
        let json = """
        {
          "protocolVersion": 1,
          "productVersion": "0.2608141625.0",
          "capabilities": {
            "create": true,
            "openViewer": true,
            "localAttach": true,
            "cloudAttach": false,
            "selfUpdate": true,
            "extensions": []
          },
          "endpoints": {
            "sessions": true,
            "sessionCapabilities": true,
            "terminal": true,
            "events": true,
            "send": true
          },
          "features": { "idempotencyKeys": true, "readOnlyTerminal": true },
          "gatewayInstanceId": "gw-1a2b3c-4d5e6f"
        }
        """
        let capabilities = try decoder.decode(
            GatewayCapabilities.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(capabilities.protocolVersion, 1)
        XCTAssertEqual(capabilities.productVersion, "0.2608141625.0")
        XCTAssertTrue(capabilities.endpoints.events)
        XCTAssertTrue(capabilities.features.idempotencyKeys)
        XCTAssertEqual(capabilities.gatewayInstanceId, "gw-1a2b3c-4d5e6f")
    }

    func testAdditiveDiscoveryFieldsAreIgnoredRatherThanFatal() throws {
        // `/v1` promises additions are additive. A gateway from a later release
        // sending a field and a flag this build has never heard of must not
        // break discovery — otherwise the app dies on every CLI update.
        let json = """
        {
          "protocolVersion": 1,
          "productVersion": "9.9.9",
          "endpoints": {
            "sessions": true, "sessionCapabilities": true, "terminal": true,
            "events": true, "send": true, "attachments": true
          },
          "features": { "idempotencyKeys": true, "readOnlyTerminal": false, "voice": true },
          "gatewayInstanceId": "gw-ff-00",
          "regions": ["us-east"]
        }
        """
        let capabilities = try decoder.decode(
            GatewayCapabilities.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(capabilities.endpoints.events)
        XCTAssertFalse(capabilities.features.readOnlyTerminal)
    }

    func testMissingOptionalMapsMeanUnavailableNotFailure() throws {
        let json = """
        { "protocolVersion": 1, "productVersion": "1.0.0", "gatewayInstanceId": "gw-1-1" }
        """
        let capabilities = try decoder.decode(
            GatewayCapabilities.self,
            from: Data(json.utf8)
        )
        XCTAssertFalse(capabilities.endpoints.send)
        XCTAssertFalse(capabilities.features.idempotencyKeys)
    }

    func testDecodesEveryDocumentedHarnessEventType() throws {
        let events = [
            #"{"type":"user_message","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"text":"hi"}"#,
            #"{"type":"assistant_delta","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"text":"he"}"#,
            #"{"type":"assistant_message","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"text":"hello"}"#,
            #"{"type":"tool_started","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"tool":"Bash","input":{"command":"ls"}}"#,
            #"{"type":"tool_finished","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"tool":"Bash","output":"a b"}"#,
            #"{"type":"awaiting_input","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"requestId":"r1","kind":"permission","prompt":"Run ls?","choices":["yes","no"]}"#,
            #"{"type":"status","sessionId":"s","at":"t","harnessVersion":"1","connectorEpoch":1,"status":"idle"}"#
        ]
        let decoded = try events.map {
            try decoder.decode(HarnessEvent.self, from: Data($0.utf8))
        }
        XCTAssertEqual(decoded.map(\.payload.wireType), LatchContract.harnessEventTypes)
    }

    func testAwaitingInputCarriesItsChoices() throws {
        let json = #"""
        {"type":"awaiting_input","sessionId":"s","at":"t","harnessVersion":"1",
         "connectorEpoch":2,"requestId":"req-7","kind":"question","prompt":"Which?",
         "choices":["a","b","c"]}
        """#
        let event = try decoder.decode(HarnessEvent.self, from: Data(json.utf8))
        guard case .awaitingInput(let requestId, let kind, let prompt, let choices) = event.payload
        else { return XCTFail("expected awaiting_input") }
        XCTAssertEqual(requestId, "req-7")
        XCTAssertEqual(kind, .question)
        XCTAssertEqual(prompt, "Which?")
        XCTAssertEqual(choices, ["a", "b", "c"])
    }

    func testUnknownEventTypeIsRetainedNotDropped() throws {
        // A new event type is an additive change a v1 gateway is allowed to
        // make. Failing to decode would silently truncate the transcript, so
        // the type is preserved and the UI shows it as unrecognized.
        let json = #"""
        {"type":"thinking","sessionId":"s","at":"t","harnessVersion":"2","connectorEpoch":1,
         "text":"…"}
        """#
        let event = try decoder.decode(HarnessEvent.self, from: Data(json.utf8))
        XCTAssertEqual(event.payload, .unknown(type: "thinking"))
        XCTAssertEqual(event.payload.wireType, "thinking")
        XCTAssertEqual(event.connectorEpoch, 1)
    }

    func testUnknownAwaitingInputKindDoesNotDiscardTheEvent() throws {
        let json = #"""
        {"type":"awaiting_input","sessionId":"s","at":"t","harnessVersion":"1",
         "connectorEpoch":1,"requestId":"r","kind":"elicitation","prompt":"?"}
        """#
        let event = try decoder.decode(HarnessEvent.self, from: Data(json.utf8))
        guard case .awaitingInput(_, let kind, _, _) = event.payload else {
            return XCTFail("expected awaiting_input")
        }
        XCTAssertNil(kind, "an unrecognized kind degrades to nil, it does not fail decoding")
    }

    func testDecodesTheSessionList() throws {
        // The session list is the CLI's snake_case report, unlike the
        // camelCase discovery document. Both conventions are the contract.
        let json = """
        {"sessions":[{
          "id":"ses_1a00","name":"agent-1","state":"running","cwd":"/Users/x/dev/Latch",
          "command_label":"claude","created_at":"2026-08-14T10:00:00Z",
          "last_activity_at":"2026-08-14T10:05:00Z","idle_ms":4000
        }]}
        """
        let report = try decoder.decode(ListReport.self, from: Data(json.utf8))
        let session = try XCTUnwrap(report.sessions.first)
        XCTAssertEqual(session.id, "ses_1a00")
        XCTAssertEqual(session.commandLabel, "claude")
        XCTAssertEqual(session.directoryName, "Latch")
        XCTAssertEqual(session.displayName, "agent-1")
        XCTAssertTrue(session.isRunning)
    }

    func testSessionCapabilitiesFlattenInteractionFields() throws {
        let json = """
        {"sendMessage":true,"sendKeys":true,"resolve":true,
         "canSend":{"ok":false,"reason":"the composer already contains text"},
         "events":{"ok":true,"harness":"claude-code","connectorEpoch":3}}
        """
        let capabilities = try decoder.decode(SessionCapabilities.self, from: Data(json.utf8))
        XCTAssertTrue(capabilities.interaction.sendMessage)
        XCTAssertFalse(capabilities.interaction.canSend.ok)
        XCTAssertEqual(
            capabilities.interaction.canSend.reason,
            "the composer already contains text"
        )
        XCTAssertEqual(capabilities.events.connectorEpoch, 3)
    }

    func testSendBodiesMatchTheRequestSchema() throws {
        XCTAssertEqual(SendOperation.message("hi").body, ["message": .string("hi")])
        XCTAssertEqual(SendOperation.keys("C-c").body, ["keys": .string("C-c")])
        XCTAssertEqual(
            SendOperation.resolve(requestId: "r", choice: "yes").body,
            ["resolve": .object(["requestId": .string("r"), "choice": .string("yes")])]
        )
        XCTAssertEqual(
            LatchContract.sendOperations,
            ["message", "keys", "resolve"],
            "the generated operation list must match the schema's oneOf order"
        )
    }

    func testOnlyRetryableOperationsTakeAnIdempotencyKey() {
        // The gateway rejects the header on `keys`, which has no retry
        // contract, so sending one would turn a keypress into a 400.
        XCTAssertTrue(SendOperation.message("hi").acceptsIdempotencyKey)
        XCTAssertTrue(SendOperation.resolve(requestId: "r", choice: "y").acceptsIdempotencyKey)
        XCTAssertFalse(SendOperation.keys("C-c").acceptsIdempotencyKey)
    }

    func testGeneratedIdempotencyKeysSatisfyTheSchema() throws {
        let pattern = try NSRegularExpression(pattern: LatchContract.idempotencyKeyPattern)
        for _ in 0..<32 {
            let key = LatchGateway.newIdempotencyKey()
            XCTAssertLessThanOrEqual(key.count, LatchContract.idempotencyKeyMaxLength)
            XCTAssertFalse(key.isEmpty)
            let range = NSRange(key.startIndex..., in: key)
            XCTAssertNotNil(
                pattern.firstMatch(in: key, range: range),
                "\(key) does not match the contract's key pattern"
            )
        }
    }

    func testTerminalModesUseTheirWireSpelling() {
        XCTAssertEqual(TerminalAccessMode.control.rawValue, "control")
        XCTAssertEqual(TerminalAccessMode.readOnly.rawValue, "read-only")
    }
}
