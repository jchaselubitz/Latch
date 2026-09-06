import XCTest

@testable import LatchMobileKit

@MainActor
final class NewSessionFolderTests: XCTestCase {
    private enum LostResponse: Error { case once }

    func testDefaultFolderStoresNilAndCanonicalPaths() {
        let suite = "NewSessionFolderTests-\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = UserDefaultsNewSessionFolderStore(defaults: defaults)

        XCTAssertNil(store.load())
        store.save("/Users/jake/Development")
        XCTAssertEqual(store.load(), "/Users/jake/Development")
        store.save(nil)
        XCTAssertNil(store.load())
    }

    func testSavedDefaultLoadsButAOneOffCreateChoiceDoesNotReplaceIt() async {
        let store = MemoryNewSessionFolderStore("/saved")
        let model = browser(mode: .create, initialPath: store.load()) { path, _ in
            self.page(path ?? "/home")
        }

        await model.load()
        await model.navigate(to: "/one-off")

        XCTAssertEqual(model.currentPage?.path, "/one-off")
        XCTAssertEqual(store.load(), "/saved")
    }

    func testUnavailableSavedDefaultExplainsAndFallsBackToHome() async {
        var paths: [String?] = []
        let model = browser(mode: .create, initialPath: "/moved") { path, _ in
            paths.append(path)
            if path == "/moved" {
                throw LatchError.http(status: 404, path: "/v2/directories", reason: "directory is unavailable")
            }
            return self.page("/Users/jake")
        }

        await model.load()

        XCTAssertEqual(paths.count, 2)
        XCTAssertEqual(paths[0], "/moved")
        XCTAssertNil(paths[1])
        XCTAssertEqual(model.currentPage?.path, "/Users/jake")
        XCTAssertNotNil(model.notice)
    }

    func testPaginationKeepsTheListingAndNavigationStack() async {
        let model = browser(mode: .create) { path, cursor in
            if cursor != nil {
                return self.page("/work", entries: [.init(name: "two", path: "/work/two")])
            }
            if path == "/work" {
                return self.page("/work", entries: [.init(name: "one", path: "/work/one")], cursor: "next")
            }
            return self.page("/home")
        }
        await model.load()
        await model.navigate(to: "/work")
        await model.loadMore()

        XCTAssertEqual(model.navigationStack, ["/home"])
        XCTAssertEqual(model.entries.map(\.name), ["one", "two"])
        XCTAssertNil(model.currentPage?.nextCursor)
    }

    func testLostCreationResponseRetriesTheSameRequestIDAndCancellationEndsIt() async {
        var requestIDs: [UUID] = []
        var calls = 0
        let model = FolderBrowserModel(
            mode: .create,
            initialPath: nil,
            folderStore: MemoryNewSessionFolderStore(),
            browse: { _, _ in self.page("/work") },
            create: { requestID, _ in
                requestIDs.append(requestID)
                calls += 1
                if calls == 1 { throw LostResponse.once }
                return self.report("ses_new")
            }
        )
        await model.load()

        await model.startSession()
        let pending = model.pendingCreationRequestID
        await model.startSession()

        XCTAssertEqual(requestIDs.count, 2)
        XCTAssertEqual(requestIDs[0], requestIDs[1])
        XCTAssertEqual(pending, requestIDs[0])
        XCTAssertEqual(model.createdSessionID, "ses_new")
        XCTAssertNil(model.pendingCreationRequestID)

        let cancelled = browser(mode: .create, create: { requestID, _ in
            requestIDs.append(requestID)
            throw LostResponse.once
        }) { _, _ in self.page("/work") }
        await cancelled.load()
        await cancelled.startSession()
        let first = cancelled.pendingCreationRequestID
        cancelled.cancelCreation()
        await cancelled.startSession()
        XCTAssertNotEqual(first, cancelled.pendingCreationRequestID)
    }

    func testGrantLossWhileOpenKeepsCurrentPageAndRefusesFurtherTraffic() async {
        var allowed = true
        var browseCalls = 0
        var createCalls = 0
        let model = FolderBrowserModel(
            mode: .create,
            initialPath: nil,
            folderStore: MemoryNewSessionFolderStore(),
            browse: { _, _ in
                browseCalls += 1
                return self.page("/home")
            },
            create: { _, _ in
                createCalls += 1
                return self.report("ses_forbidden")
            },
            hasAccess: { allowed }
        )
        await model.load()
        allowed = false

        await model.navigate(to: "/home/secret")
        await model.startSession()

        XCTAssertEqual(browseCalls, 1)
        XCTAssertEqual(createCalls, 0)
        XCTAssertEqual(model.currentPage?.path, "/home")
        XCTAssertNotNil(model.error)
    }

    func testChoosingDefaultSavesOnlyInSelectionMode() async {
        let store = MemoryNewSessionFolderStore()
        let model = FolderBrowserModel(
            mode: .chooseDefault,
            initialPath: nil,
            folderStore: store,
            browse: { _, _ in self.page("/chosen") },
            create: { _, _ in self.report("unused") }
        )
        await model.load()

        XCTAssertTrue(model.useCurrentAsDefault())
        XCTAssertEqual(store.load(), "/chosen")
    }

    private func browser(
        mode: FolderBrowserMode,
        initialPath: String? = nil,
        create: @escaping FolderBrowserModel.Creating = { _, _ in
            throw LostResponse.once
        },
        browse: @escaping FolderBrowserModel.Browsing
    ) -> FolderBrowserModel {
        FolderBrowserModel(
            mode: mode,
            initialPath: initialPath,
            folderStore: MemoryNewSessionFolderStore(initialPath),
            browse: browse,
            create: create
        )
    }

    private func page(
        _ path: String,
        entries: [DirectoryEntry] = [],
        cursor: String? = nil
    ) -> DirectoryPage {
        DirectoryPage(path: path, parent: path == "/" ? nil : "/", entries: entries, nextCursor: cursor)
    }

    private func report(_ id: String) -> CreateReport {
        CreateReport(
            protocolVersion: 2,
            session: CreatedSession(id: id, name: "shell", state: "running", createdAt: "2026-09-06T00:00:00Z")
        )
    }
}
