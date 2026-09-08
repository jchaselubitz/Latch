import XCTest

@testable import LatchMobileKit

@MainActor
final class NewSessionAppModelTests: XCTestCase {
    private static let capabilities = """
    {"protocolVersion":2,"productVersion":"2.0.0",
     "capabilities":{"create":true,"openViewer":true,"localAttach":true,
      "cloudAttach":false,"selfUpdate":true,"extensions":[]},
     "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
      "browseDirectories":true,"createSession":true},
     "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
     "operationRetentionSeconds":600}
    """

    override func setUp() {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities)
        StubProtocol.stub(path: "/v2/sessions", body: #"{"sessions":[]}"#)
        StubProtocol.stub(
            path: "/v2/directories",
            body: #"{"path":"/home","parent":"/","entries":[],"nextCursor":null}"#
        )
        StubProtocol.stub(
            path: "/v2/sessions",
            body: #"{"sessions":[]}"#
        )
    }

    func testBrowserRequiresOwnerAndSharesItsGraceWindowWithTerminal() async throws {
        let authenticator = StubDeviceOwnerAuthenticator()
        let model = makePairedModel(authenticator: authenticator)
        await connect(model)

        let browser = await model.newSessionFolderBrowser(mode: .create)

        XCTAssertNotNil(browser)
        XCTAssertEqual(authenticator.prompts, 1)
        let terminalUnlocked = await model.unlockTerminal()
        XCTAssertTrue(terminalUnlocked)
        XCTAssertEqual(authenticator.prompts, 1, "the five-minute owner check is shared")
    }

    func testCancelledOwnerCheckNeverConstructsOrFetchesABrowser() async {
        let model = makePairedModel(
            authenticator: StubDeviceOwnerAuthenticator(approves: false)
        )
        await connect(model)
        let requestsBefore = StubProtocol.requests.count

        let browser = await model.newSessionFolderBrowser(mode: .create)

        XCTAssertNil(browser)
        XCTAssertEqual(StubProtocol.requests.count, requestsBefore)
    }

    func testCreationRefreshesSessionsHighlightsResultAndDoesNotOpenATerminal() async throws {
        let terminalConnections = LockedCounter()
        let model = makePairedModel(
            authenticator: StubDeviceOwnerAuthenticator(),
            terminalConnector: { _, _, _ in
                terminalConnections.increment()
                throw LatchError.transport("must not attach")
            }
        )
        await connect(model)
        StubProtocol.stub(
            method: "POST",
            path: "/v2/sessions",
            body: """
            {"protocolVersion":2,"session":{"id":"ses_new","name":"shell",
            "state":"running","createdAt":"2026-09-06T00:00:00Z"}}
            """
        )
        let openedBrowser = await model.newSessionFolderBrowser(mode: .create)
        let browser = try XCTUnwrap(openedBrowser)
        let sessionRequestsBefore = StubProtocol.requests.filter { $0.path == "/v2/sessions" }.count

        await browser.startSession()

        XCTAssertEqual(browser.createdSessionID, "ses_new")
        XCTAssertEqual(model.highlightedSessionID, "ses_new")
        XCTAssertEqual(terminalConnections.value, 0)
        XCTAssertEqual(
            StubProtocol.requests.filter { $0.path == "/v2/sessions" }.count,
            sessionRequestsBefore + 2,
            "one POST creates and one GET refreshes"
        )
    }

    func testPairedGrantDowngradeDisablesAnOpenBrowserBeforeItsNextRequest() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )
        let model = AppModel(
            pairedGatewayFactory: { _ in gateway },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        let control = pairedRecord(permission: .control)
        await model.connectPairedDevice(control)
        XCTAssertTrue(model.canCreateNewSession)
        let openedBrowser = await model.newSessionFolderBrowser(mode: .create)
        let browser = try XCTUnwrap(openedBrowser)
        let requestsBefore = StubProtocol.requests.count

        XCTAssertTrue(model.applyPairedDeviceRecord(control.updating(permission: .interact)))
        XCTAssertFalse(model.canBrowseNewSessionFolders)
        XCTAssertFalse(model.canCreateNewSession)
        await browser.navigate(to: "/home/secret")

        XCTAssertEqual(StubProtocol.requests.count, requestsBefore)
        XCTAssertEqual(browser.currentPage?.path, "/home")
        XCTAssertNotNil(browser.error)
    }

    /// The toolbar control stays visible when the Mac serves both routes and
    /// only the grant is missing, so the app can say what to change rather
    /// than removing the button and explaining nothing.
    func testAdvertisedCreationSurvivesAGrantDowngradeAndCarriesAnExplanation() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )
        let model = AppModel(
            pairedGatewayFactory: { _ in gateway },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        let control = pairedRecord(permission: .control)
        await model.connectPairedDevice(control)

        XCTAssertTrue(model.advertisesNewSessionCreation)
        XCTAssertNil(model.newSessionUnavailableExplanation)

        XCTAssertTrue(model.applyPairedDeviceRecord(control.updating(permission: .observe)))

        XCTAssertTrue(model.advertisesNewSessionCreation)
        XCTAssertFalse(model.canCreateNewSession)
        XCTAssertNotNil(model.newSessionUnavailableExplanation)
    }

    /// A Mac that predates the feature advertises neither route, so there is
    /// nothing to show and nothing to explain.
    func testPreFeatureGatewayAdvertisesNoCreationAndOffersNoExplanation() async {
        StubProtocol.stub(
            path: "/v2/capabilities",
            body: """
            {"protocolVersion":2,"productVersion":"1.9.0",
             "capabilities":{"create":true,"openViewer":true,"localAttach":true,
              "cloudAttach":false,"selfUpdate":true,"extensions":[]},
             "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true},
             "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-old",
             "operationRetentionSeconds":600}
            """
        )
        let model = makePairedModel(authenticator: StubDeviceOwnerAuthenticator())
        await connect(model)

        XCTAssertFalse(model.advertisesNewSessionCreation)
        XCTAssertFalse(model.canBrowseNewSessionFolders)
        XCTAssertNil(model.newSessionUnavailableExplanation)
        let browser = await model.newSessionFolderBrowser(mode: .create)
        XCTAssertNil(browser)
    }

    /// Settings reads the default from the model, so choosing one has to be
    /// visible without relaunching, and clearing it has to return the picker
    /// to the Mac's home directory.
    func testDefaultFolderMirrorsTheStoreAfterSelectionAndClearing() async throws {
        let store = MemoryNewSessionFolderStore()
        let model = AppModel(
            pairedGatewayFactory: { _ in
                LatchGateway(
                    link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
                    session: StubProtocol.session()
                )
            },
            newSessionFolderStore: store,
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
        await connect(model)
        XCTAssertNil(model.defaultNewSessionFolder)

        let openedBrowser = await model.newSessionFolderBrowser(mode: .chooseDefault)
        let browser = try XCTUnwrap(openedBrowser)
        XCTAssertTrue(browser.useCurrentAsDefault())
        model.reloadDefaultNewSessionFolder()
        XCTAssertEqual(model.defaultNewSessionFolder, "/home")

        model.clearDefaultNewSessionFolder()
        XCTAssertNil(model.defaultNewSessionFolder)
        XCTAssertNil(store.load())
    }

    /// The highlight is a pointer at a row, not a session the phone holds. It
    /// must not survive an unlink.
    func testUnlinkClearsTheCreatedSessionHighlight() async throws {
        let model = makePairedModel(authenticator: StubDeviceOwnerAuthenticator())
        await connect(model)
        StubProtocol.stub(
            method: "POST",
            path: "/v2/sessions",
            body: """
            {"protocolVersion":2,"session":{"id":"ses_new","name":"shell",
            "state":"running","createdAt":"2026-09-06T00:00:00Z"}}
            """
        )
        let openedBrowser = await model.newSessionFolderBrowser(mode: .create)
        let browser = try XCTUnwrap(openedBrowser)
        await browser.startSession()
        XCTAssertEqual(model.highlightedSessionID, "ses_new")

        model.unlink()

        XCTAssertNil(model.highlightedSessionID)
    }

    private func makePairedModel(
        authenticator: StubDeviceOwnerAuthenticator,
        terminalConnector: AppModel.TerminalConnecting? = nil
    ) -> AppModel {
        AppModel(
            pairedGatewayFactory: { _ in
                LatchGateway(
                    link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
                    session: StubProtocol.session()
                )
            },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalConnector: terminalConnector,
            terminalUnlock: TerminalUnlock(authenticator: authenticator, grace: 600)
        )
    }

    private func connect(_ model: AppModel) async {
        await model.connectPairedDevice(pairedRecord(permission: .control))
    }

    private func pairedRecord(permission: DevicePermission) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(
                deviceId: "mac",
                publicKey: String(repeating: "22", count: 32),
                name: "Mac"
            ),
            permission: permission,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        )
    }
}

private final class LockedCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func increment() { lock.withLock { count += 1 } }
}
