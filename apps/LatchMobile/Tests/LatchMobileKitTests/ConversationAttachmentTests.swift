import XCTest

@testable import LatchMobileKit

/// Attachments by workspace file handoff: upload the file to the Mac, then
/// send a message that names it by path.
///
/// The ordering is the property that matters. An agent that reads a path
/// before the file exists fails in a way the person cannot see from the
/// phone, so nothing is sent until every upload has landed, and a failure at
/// any step sends nothing and loses nothing.
@MainActor
final class ConversationAttachmentTests: XCTestCase {
    /// Records the order of uploads and can be told to fail one of them.
    private actor RecordingUploader: ConversationAttachmentUploading {
        private(set) var uploaded: [String] = []
        private var failures: [String: LatchError] = [:]

        func fail(_ name: String, with error: LatchError) { failures[name] = error }
        func succeed(_ name: String) { failures[name] = nil }

        func uploadAttachment(sessionID: String, name: String, data: Data) async throws -> AttachmentReceipt {
            if let error = failures[name] { throw error }
            uploaded.append(name)
            return AttachmentReceipt(
                path: "/Users/person/work/.latch-attachments/\(name)",
                relativePath: ".latch-attachments/\(name)",
                name: name,
                bytes: data.count
            )
        }
    }

    private final class MemoryStorage: ConversationStoreStorage, @unchecked Sendable {
        func load(sessionID: String) throws -> ConversationStoreCache? { nil }
        func save(_ cache: ConversationStoreCache, sessionID: String) throws {}
    }

    private func store(uploader: RecordingUploader, canSend: Bool = true) throws -> ConversationStore {
        let store = ConversationStore(
            sessionID: "ses_1",
            gateway: LatchGateway(link: try GatewayLink(address: "http://127.0.0.1:8787", token: "")),
            operationRetentionSeconds: 60,
            storage: MemoryStorage(),
            attachmentUploader: uploader
        )
        store.receive(.message(.snapshot(ConversationSnapshot(
            generation: "g",
            revision: 1,
            operationEpoch: "epoch",
            items: [],
            state: state(canSend: canSend),
            hasMoreBefore: false,
            reason: "generation"
        ))))
        return store
    }

    private func state(canSend: Bool) -> ConversationState {
        ConversationState(
            phase: "ready",
            sendMessage: OperationAvailability(enabled: canSend, reason: canSend ? nil : "The agent is working."),
            resolveRequest: OperationAvailability(enabled: false, reason: nil),
            pendingRequest: nil,
            connector: ConnectorIdentity(id: "claude", version: "1")
        )
    }

    private func file(_ name: String, bytes: Int = 4) -> ConversationAttachment {
        ConversationAttachment(name: name, data: Data(repeating: 1, count: bytes), kind: .image)
    }

    func testUploadsEveryFileBeforeSendingOneMessageThatNamesThem() async throws {
        let uploader = RecordingUploader()
        let store = try store(uploader: uploader)
        XCTAssertTrue(store.addAttachment(file("a.png"), maximumBytes: 100))
        XCTAssertTrue(store.addAttachment(file("b.pdf"), maximumBytes: 100))

        store.send(text: "What is in these?")
        XCTAssertEqual(store.attachmentPhase, .uploading)
        XCTAssertTrue(store.operations.isEmpty, "nothing is sent before the files land")
        await store.attachmentSendTask?.value

        let uploaded = await uploader.uploaded
        XCTAssertEqual(uploaded, ["a.png", "b.pdf"])
        XCTAssertEqual(store.operations.count, 1)
        XCTAssertEqual(
            store.operations.first?.text,
            """
            What is in these?

            Attached files:
            - /Users/person/work/.latch-attachments/a.png
            - /Users/person/work/.latch-attachments/b.pdf
            """
        )
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(store.attachmentPhase, .idle)
    }

    func testAFileAloneIsAMessage() async throws {
        let uploader = RecordingUploader()
        let store = try store(uploader: uploader)
        store.addAttachment(file("shot.png"), maximumBytes: nil)

        store.send(text: "   ")
        await store.attachmentSendTask?.value

        XCTAssertEqual(
            store.operations.first?.text,
            "Attached file: /Users/person/work/.latch-attachments/shot.png"
        )
    }

    /// A failed upload sends nothing, puts the text back, and keeps the files.
    /// The retry uploads only what did not arrive the first time.
    func testAFailedUploadSendsNothingAndARetryUploadsOnlyWhatIsMissing() async throws {
        let uploader = RecordingUploader()
        await uploader.fail("b.pdf", with: .transport("The connection was lost."))
        let store = try store(uploader: uploader)
        store.addAttachment(file("a.png"), maximumBytes: nil)
        store.addAttachment(file("b.pdf"), maximumBytes: nil)
        store.draft = ""

        store.send(text: "Look")
        await store.attachmentSendTask?.value

        XCTAssertTrue(store.operations.isEmpty)
        XCTAssertEqual(store.draft, "Look")
        XCTAssertEqual(store.attachments.map(\.name), ["a.png", "b.pdf"])
        XCTAssertNotNil(store.attachments[0].receipt)
        XCTAssertNil(store.attachments[1].receipt)
        guard case .failed(let reason) = store.attachmentPhase else {
            return XCTFail("the failure is reported")
        }
        XCTAssertTrue(reason.contains("did not reach your Mac"), reason)

        await uploader.succeed("b.pdf")
        let text = store.draft
        store.draft = ""
        store.send(text: text)
        await store.attachmentSendTask?.value

        let uploaded = await uploader.uploaded
        XCTAssertEqual(uploaded, ["a.png", "b.pdf"], "a.png is not uploaded twice")
        XCTAssertEqual(store.operations.count, 1)
        XCTAssertTrue(store.attachments.isEmpty)
    }

    /// A refusal from the Mac is already a sentence; it is shown as is.
    func testARefusalFromTheMacIsShownAsItsOwnSentence() async throws {
        let uploader = RecordingUploader()
        await uploader.fail("a.png", with: .refused("attachments are limited to 25 MB"))
        let store = try store(uploader: uploader)
        store.addAttachment(file("a.png"), maximumBytes: nil)

        store.send(text: "x")
        await store.attachmentSendTask?.value

        XCTAssertEqual(store.attachmentPhase, .failed("attachments are limited to 25 MB"))
    }

    /// The agent can become busy while files upload. The files stay on the
    /// Mac and in the composer, and the text returns to the draft.
    func testAnAgentThatBecameBusyDuringTheUploadGetsNoMessage() async throws {
        let uploader = RecordingUploader()
        let store = try store(uploader: uploader)
        store.addAttachment(file("a.png"), maximumBytes: nil)

        store.send(text: "Now")
        store.receive(.message(.stateChanged(generation: "g", revision: 2, state: state(canSend: false))))
        await store.attachmentSendTask?.value

        XCTAssertTrue(store.operations.isEmpty)
        XCTAssertEqual(store.draft, "Now")
        XCTAssertEqual(store.attachments.count, 1)
        XCTAssertNotNil(store.attachments.first?.receipt)
        XCTAssertEqual(store.attachmentPhase, .failed("The agent is working."))
    }

    func testNothingIsUploadedWhenTheAgentCannotTakeAMessage() async throws {
        let uploader = RecordingUploader()
        let store = try store(uploader: uploader, canSend: false)
        store.addAttachment(file("a.png"), maximumBytes: nil)

        store.send(text: "Hello")
        await store.attachmentSendTask?.value

        let uploaded = await uploader.uploaded
        XCTAssertTrue(uploaded.isEmpty)
        XCTAssertEqual(store.draft, "Hello")
        XCTAssertEqual(store.attachments.count, 1)
    }

    func testAFileOverTheMacsLimitIsRefusedBeforeUploading() throws {
        let store = try store(uploader: RecordingUploader())
        XCTAssertFalse(store.addAttachment(file("big.mov", bytes: 11), maximumBytes: 10))
        XCTAssertTrue(store.attachments.isEmpty)
        guard case .failed(let reason) = store.attachmentPhase else {
            return XCTFail("the refusal is explained")
        }
        XCTAssertTrue(reason.contains("big.mov"), reason)
    }

    func testFilesCannotChangeWhileUploading() async throws {
        let store = try store(uploader: RecordingUploader())
        store.addAttachment(file("a.png"), maximumBytes: nil)
        store.send(text: "x")
        XCTAssertFalse(store.addAttachment(file("b.png"), maximumBytes: nil))
        store.removeAttachment(store.attachments[0].id)
        XCTAssertEqual(store.attachments.count, 1)
        await store.attachmentSendTask?.value
    }

    func testRemovingAFileClearsAnEarlierFailure() throws {
        let store = try store(uploader: RecordingUploader())
        store.addAttachment(file("big.mov", bytes: 11), maximumBytes: 10)
        store.addAttachment(file("a.png"), maximumBytes: 10)
        store.removeAttachment(store.attachments[0].id)
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(store.attachmentPhase, .idle)
    }

    func testSuggestedNamesStayInsideTheMacsAlphabet() {
        XCTAssertEqual(ConversationAttachment.suggestedName("IMG_0001.HEIC"), "IMG_0001.HEIC")
        XCTAssertEqual(ConversationAttachment.suggestedName("../../etc/passwd"), "passwd")
        XCTAssertEqual(ConversationAttachment.suggestedName("my report (final).pdf"), "my-report-final-.pdf")
        XCTAssertEqual(ConversationAttachment.suggestedName("a..b.txt"), "a.b.txt")
        XCTAssertEqual(ConversationAttachment.suggestedName("résumé.docx"), "r-sum-.docx")
        XCTAssertEqual(ConversationAttachment.suggestedName("..."), "attachment")
        XCTAssertEqual(ConversationAttachment.suggestedName(""), "attachment")
        XCTAssertFalse(ConversationAttachment.suggestedName("x..y..z").contains(".."))
    }
}

/// The upload request itself, and when the phone may make it at all.
@MainActor
final class AttachmentGatewayTests: XCTestCase {
    override func setUp() {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(attachments: true))
        StubProtocol.stub(
            method: "POST",
            path: "/v2/sessions/ses_1/attachments",
            status: 201,
            body: #"{"path":"/Users/person/work/.latch-attachments/a.png","relativePath":".latch-attachments/a.png","name":"a.png","bytes":3}"#
        )
    }

    func testUploadPostsTheRawFileWithASanitizedName() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        let receipt = try await gateway.uploadAttachment(
            sessionID: "ses_1",
            name: "../my photo.png",
            data: Data("abc".utf8)
        )

        XCTAssertEqual(receipt.name, "a.png")
        XCTAssertEqual(receipt.path, "/Users/person/work/.latch-attachments/a.png")
        let request = try XCTUnwrap(StubProtocol.requests.last)
        XCTAssertEqual(request.method, "POST")
        XCTAssertEqual(request.path, "/v2/sessions/ses_1/attachments")
        XCTAssertEqual(request.query, "name=my-photo.png")
        XCTAssertEqual(request.headers["Content-Type"], "application/octet-stream")
        XCTAssertEqual(request.body, "abc")
    }

    func testAMacWithoutTheRouteIsNeverAskedAndHidesAttachments() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(attachments: false))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )

        do {
            _ = try await gateway.uploadAttachment(sessionID: "ses_1", name: "a.png", data: Data("a".utf8))
            XCTFail("the upload should be gated by discovery")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.attachments))
        }
        XCTAssertEqual(StubProtocol.requests.map(\.path), ["/v2/capabilities"])

        let capabilities = try await gateway.discover()
        XCTAssertFalse(GatewayCompatibility.sessionSurface(for: capabilities).attachments)
        XCTAssertNil(capabilities.features.attachmentMaxBytes)
    }

    func testAttachmentsFollowTheComposerGrant() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )
        let capabilities = try await gateway.discover()
        XCTAssertEqual(capabilities.features.attachmentMaxBytes, 26_214_400)
        let surface = GatewayCompatibility.sessionSurface(for: capabilities)
        XCTAssertTrue(surface.attachments)
        XCTAssertFalse(surface.restricted(to: .observe).attachments)
        XCTAssertFalse(surface.restricted(to: nil).attachments)
        XCTAssertTrue(surface.restricted(to: .interact).attachments)
        XCTAssertTrue(surface.restricted(to: .control).attachments)
    }

    func testTheMacsRefusalReasonIsKept() {
        let error = LatchGateway.error(
            status: 413,
            path: "/v2/sessions/ses_1/attachments",
            data: Data(#"{"error":"attachment_too_large","reason":"attachments are limited to 25 MB"}"#.utf8)
        )
        XCTAssertEqual(error, .refused("attachments are limited to 25 MB"))
    }

    private static func capabilities(attachments: Bool) -> String {
        let limit = attachments ? #","attachmentMaxBytes":26214400"# : ""
        return """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true,"stopSession":true,"attachments":\(attachments)},
         "features":{"exclusiveTerminal":true\(limit)},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }
}
