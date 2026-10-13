import XCTest

@testable import LatchMobileKit

/// Slice 1 of Git management: read-only repository selection, status and
/// diffs. Every response here has the shape the Mac's gateway sends.
@MainActor
final class GitManagementTests: XCTestCase {
    private static func capabilities(git: Bool) -> String {
        let gitKeys = git
            ? #","gitRepositories":true,"gitResolve":true,"gitStatus":true,"gitDiff":true"#
            : ""
        return """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true\(gitKeys)},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }

    static let status = """
    {"root":"/Users/person/src/latch","name":"latch","kind":"linked_worktree",
     "head":{"branch":"git-ui","oid":"0123456789abcdef0123456789abcdef01234567","detached":false},
     "upstream":{"name":"origin/git-ui","ahead":2,"behind":1},
     "lastFetchedAt":"2026-10-13T10:00:00Z",
     "operation":{"kind":"rebase","step":3,"total":5},
     "entries":[
      {"path":"src/README.md","pathKey":"7372632f524541444d452e6d64","origPath":null,"origPathKey":null,
       "area":"conflicted","change":"conflicted","conflict":"UU","submodule":false,
       "additions":null,"deletions":null,"binary":false},
      {"path":"App/FolderPickerView.swift","pathKey":"6170702f66","origPath":"App/Old.swift","origPathKey":"61","area":"staged",
       "change":"renamed","conflict":null,"submodule":false,"additions":12,"deletions":4,"binary":false},
      {"path":"with space\\nnewline.txt","pathKey":"77","origPath":null,"origPathKey":null,"area":"unstaged",
       "change":"modified","conflict":null,"submodule":false,"additions":1,"deletions":0,"binary":false},
      {"path":"image.png","pathKey":"69","origPath":null,"origPathKey":null,"area":"unstaged",
       "change":"modified","conflict":null,"submodule":false,"additions":null,"deletions":null,"binary":true},
      {"path":"notes/","pathKey":"6e","origPath":null,"origPathKey":null,"area":"untracked",
       "change":"untracked","conflict":null,"submodule":false,"additions":null,"deletions":null,"binary":false},
      {"path":"future.txt","pathKey":"66","origPath":null,"origPathKey":null,"area":"unstaged",
       "change":"some_future_kind","conflict":null,"submodule":false,"additions":0,"deletions":0,"binary":false}
     ],
     "entriesTruncated":false,"totalEntries":6,
     "sessions":[{"id":"ses_1","name":"git-ui","title":null,"commandLabel":"claude","connector":"claude",
       "agentPhase":"working","idleMs":100,"active":true},
      {"id":"ses_2","name":"zsh","title":null,"commandLabel":"zsh","connector":null,
       "agentPhase":null,"idleMs":900000,"active":false}],
     "observedAt":"2026-10-13T10:04:00Z"}
    """

    static func diffPage(hunks: [Int], total: Int, next: String?) -> String {
        let rendered = hunks.map { index in
            """
            {"index":\(index),"header":"@@ -\(index * 10 + 1),2 +\(index * 10 + 1),3 @@","oldStart":\(index * 10 + 1),
             "oldLines":2,"newStart":\(index * 10 + 1),"newLines":3,"omittedLines":0,
             "lines":[{"kind":"context","oldLine":\(index * 10 + 1),"newLine":\(index * 10 + 1),"text":"same"},
              {"kind":"added","oldLine":null,"newLine":\(index * 10 + 2),"text":"new","truncated":true},
              {"kind":"note","oldLine":null,"newLine":null,"text":"\\\\ No newline at end of file"}]}
            """
        }.joined(separator: ",")
        let cursor = next.map { "\"\($0)\"" } ?? "null"
        return """
        {"root":"/Users/person/src/latch","path":"with space","pathKey":"77","origPath":null,
         "area":"unstaged","kind":"text","additions":\(total),"deletions":0,
         "hunks":[\(rendered)],"totalHunks":\(total),"nextCursor":\(cursor),"notes":[]}
        """
    }

    override func setUp() {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true))
        StubProtocol.stub(path: "/v2/sessions", body: #"{"sessions":[]}"#)
        StubProtocol.stub(path: "/v2/git/status", body: Self.status)
        StubProtocol.stub(
            path: "/v2/git/repositories",
            body: """
            {"fromSessions":[{"root":"/Users/person/src/latch","name":"latch","kind":"repository",
              "sessions":[{"id":"ses_1","name":"git-ui","title":null,"commandLabel":"claude",
               "connector":"claude","agentPhase":"working","idleMs":0,"active":true}],
              "lastOpenedAt":null}],
             "recent":[{"root":"/Users/person/src/site","name":"site","kind":"submodule",
              "sessions":[],"lastOpenedAt":"2026-10-12T09:00:00Z"}]}
            """
        )
    }

    // MARK: Decoding

    func testStatusDecodesEveryFieldAndToleratesAnUnknownChangeKind() throws {
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status.utf8))
        XCTAssertEqual(status.kind, .linkedWorktree)
        XCTAssertEqual(status.head.branch, "git-ui")
        XCTAssertEqual(status.upstream?.ahead, 2)
        XCTAssertEqual(status.operation, GitOperation(kind: .rebase, step: 3, total: 5))
        XCTAssertEqual(status.entries.count, 6)
        XCTAssertEqual(status.entries[2].path, "with space\nnewline.txt")
        XCTAssertEqual(status.entries[1].origPath, "App/Old.swift")
        XCTAssertEqual(status.entries[5].change, .modified, "a newer Mac's kind does not fail the status")
        XCTAssertEqual(status.sessions.map(\.id), ["ses_1", "ses_2"])
    }

    func testDiffLinesDefaultTruncatedToFalseWhenAbsent() throws {
        let diff = try JSONDecoder().decode(
            GitFileDiff.self,
            from: Data(Self.diffPage(hunks: [0], total: 1, next: nil).utf8)
        )
        XCTAssertFalse(diff.hunks[0].lines[0].truncated)
        XCTAssertTrue(diff.hunks[0].lines[1].truncated)
        XCTAssertEqual(diff.hunks[0].lines[2].kind, .note)
    }

    // MARK: Presentation

    func testRowsSectionsAndSpokenStatusFollowTheDesign() throws {
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status.utf8))
        XCTAssertEqual(
            GitSectionGroup.grouped(status.entries).map(\.section),
            [.conflicted, .staged, .changes, .untracked]
        )
        XCTAssertEqual(status.chromeStatus, "git-ui · \u{2191}2 \u{2193}1")
        XCTAssertEqual(GitPresentation.aheadBehindSpoken(status.upstream), "2 commits to push, 1 incoming")
        let renamed = status.entries[1]
        XCTAssertEqual(renamed.badge, "R")
        XCTAssertEqual(renamed.fileName, "FolderPickerView.swift")
        XCTAssertEqual(renamed.detail, "was App/Old.swift")
        XCTAssertEqual(renamed.lineCounts, "+12 \u{2212}4")
        XCTAssertEqual(renamed.stagingLabel, "Unstage FolderPickerView.swift")
        XCTAssertEqual(status.entries[2].accessibilityStatus, "Modified, unstaged, 1 added, 0 removed")
        XCTAssertEqual(status.entries[0].badge, "!")
        XCTAssertEqual(status.entries[0].accessibilityStatus, "Conflicted")
        XCTAssertNil(status.entries[3].lineCounts, "binary files show no counts")
        XCTAssertEqual(status.entries[4].fileName, "notes/")
        XCTAssertEqual(GitPresentation.operationTitle(status.operation!), "Rebase stopped · 3 of 5")
        XCTAssertEqual(status.sessions.workingHereTitle, "2 sessions working here")
        XCTAssertEqual(status.sessions[0].chipLabel, "Claude · git-ui · working")
        XCTAssertEqual(status.sessions[1].chipLabel, "Terminal · zsh · idle")
    }

    func testTrackingLineAlwaysSaysWhenItWasLastFetched() throws {
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status.utf8))
        let now = GitPresentation.parseDate("2026-10-13T10:04:30Z")!
        XCTAssertEqual(GitPresentation.trackingLine(status, now: now), "Tracks origin/git-ui · fetched 4 min ago")
        var never = status
        never.lastFetchedAt = nil
        XCTAssertEqual(GitPresentation.trackingLine(never, now: now), "Tracks origin/git-ui · never fetched")
        var detached = status
        detached.head = GitHead(branch: nil, oid: "abcdef0123456789abcdef0123456789abcdef01", detached: true)
        detached.upstream = nil
        XCTAssertEqual(detached.branchLabel, "abcdef0")
        XCTAssertEqual(GitPresentation.trackingLine(detached, now: now), "Detached HEAD")
        XCTAssertEqual(
            GitPresentation.stalePill(loadedAt: now.addingTimeInterval(-240), now: now),
            "Reconnecting · showing what Latch saw 4 min ago"
        )
    }

    // MARK: Capability gating

    func testAnOlderMacWithoutGitRoutesOffersNoEntryPoint() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: false))
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        XCTAssertFalse(model.advertisesGit)
        XCTAssertFalse(model.canUseGit)
        XCTAssertNil(model.gitUnavailableExplanation)
        let list = await model.gitRepositoryList()
        let repository = await model.gitRepository(folder: "/Users/person/src/latch")
        XCTAssertNil(list)
        XCTAssertNil(repository)
        XCTAssertFalse(StubProtocol.requests.contains { $0.path.hasPrefix("/v2/git/") })
    }

    func testBelowControlTheEntryPointExplainsItselfAndFetchesNothing() async {
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord(permission: .interact))
        XCTAssertTrue(model.advertisesGit)
        XCTAssertFalse(model.canUseGit)
        XCTAssertNotNil(model.gitUnavailableExplanation)
        let list = await model.gitRepositoryList()
        XCTAssertNil(list)
        XCTAssertFalse(StubProtocol.requests.contains { $0.path.hasPrefix("/v2/git/") })
    }

    // MARK: Owner check

    func testOpeningARepositoryAsksTheOwnerFirstAndNeverAttaches() async throws {
        let authenticator = StubDeviceOwnerAuthenticator(approves: false)
        let terminals = Counter()
        let model = makePairedModel(authenticator: authenticator, terminalConnector: { _, _, _ in
            terminals.increment()
            throw LatchError.transport("must not attach")
        })
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let refused = await model.gitRepository(folder: "/Users/person/src/latch")
        XCTAssertNil(refused)
        XCTAssertFalse(StubProtocol.requests.contains { $0.path.hasPrefix("/v2/git/") })

        authenticator.set(approves: true)
        let opened = await model.gitRepository(folder: "/Users/person/src/latch/App")
        let repository = try XCTUnwrap(opened)
        XCTAssertEqual(repository.status?.root, "/Users/person/src/latch")
        XCTAssertEqual(repository.target, "/Users/person/src/latch")
        XCTAssertEqual(terminals.value, 0)
        let query = StubProtocol.requestQueries.last { $0.0 == "/v2/git/status" }?.1
        XCTAssertEqual(query, "path=/Users/person/src/latch/App")
    }

    func testTheRepositoriesSheetReadsTheMacsListAndStoresNothing() async throws {
        let before = Set(UserDefaults.standard.dictionaryRepresentation().keys)
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let opened = await model.gitRepositoryList()
        let list = try XCTUnwrap(opened)
        XCTAssertEqual(list.list?.fromSessions.first?.sessions.count, 1)
        XCTAssertEqual(list.list?.recent.first?.name, "site")
        // The phone writes nothing about the Mac's repositories.
        let after = UserDefaults.standard.dictionaryRepresentation()
        XCTAssertEqual(Set(after.keys), before)
        XCTAssertFalse(after.values.contains { "\($0)".contains("/Users/person/src") })
    }

    func testAnOpenRepositoryUsesTheReplacementGatewayAfterReconnect() async throws {
        let app = makePairedModel()
        let record = pairedRecord(permission: .control)
        await app.connectPairedDevice(record)
        let opened = await app.gitRepository(folder: "/Users/person/src/latch")
        let repository = try XCTUnwrap(opened)
        let oldGateway = try XCTUnwrap(app.gateway)
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: false))
        _ = try await oldGateway.discover()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true))
        await app.connectPairedDevice(record)
        await repository.reconnected()
        XCTAssertNil(repository.error, "the closed adapter must not be captured by an open screen")
        let reopened = await app.gitRepository(folder: "/Users/person/src/latch")
        XCTAssertTrue(repository === reopened, "back-navigation must preserve the runner and its request ID")
    }

    // MARK: Repository model

    func testAFailedRefreshAndAnOfflineLinkKeepTheLastStatusReadable() async throws {
        var online = true
        var fail = false
        var reads = 0
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status.utf8))
        let model = GitRepositoryModel(
            folder: "/Users/person/src/latch",
            read: { _ in
                reads += 1
                if fail { throw LatchError.git(code: "git_timeout", reason: "Git on your Mac did not answer in time.") }
                return status
            },
            readDiff: { _, _, _, _ in throw LatchError.transport("unused") },
            isOnline: { online }
        )
        await model.refresh()
        XCTAssertEqual(model.status, status)
        XCTAssertFalse(model.isStale)

        fail = true
        await model.refresh()
        XCTAssertEqual(model.status, status, "a failed refresh keeps the screen")
        XCTAssertEqual(model.error, "Git on your Mac did not answer in time.")
        XCTAssertTrue(model.isStale, "a reachable gateway with a failed status is still stale")

        online = false
        await model.refresh()
        XCTAssertEqual(reads, 2, "nothing is asked while the link is down")
        XCTAssertTrue(model.isStale)
        XCTAssertEqual(model.status, status)
        XCTAssertTrue(model.stalePill().hasPrefix("Reconnecting · showing what Latch saw"))

        online = true
        await model.reconnected()
        XCTAssertTrue(model.isStale, "reconnecting does not make the cached status fresh")
        fail = false
        await model.refresh()
        XCTAssertFalse(model.isStale)

        let entries = model.orderedEntries
        XCTAssertEqual(model.neighbor(of: entries[0], offset: 1), entries[1])
        XCTAssertNil(model.neighbor(of: entries[0], offset: -1))
    }

    // MARK: Diff model

    func testLoadMoreAppendsEachHunkOnceAndAStaleCursorStartsOver() async throws {
        var calls: [String?] = []
        var staleOnce = false
        let entry = GitStatusEntry(path: "with space", pathKey: "77", area: .unstaged, change: .modified)
        let model = GitDiffModel(root: "/r", entry: entry, read: { root, key, area, cursor in
            XCTAssertEqual(root, "/r")
            XCTAssertEqual(key, "77")
            XCTAssertEqual(area, .unstaged)
            calls.append(cursor)
            if staleOnce, cursor != nil {
                staleOnce = false
                throw LatchError.git(code: "stale_cursor", reason: "changed")
            }
            let page: String = switch cursor {
            case nil: GitManagementTests.diffPage(hunks: [0, 1], total: 3, next: "v1-ab-2")
            default: GitManagementTests.diffPage(hunks: [2], total: 3, next: nil)
            }
            return try JSONDecoder().decode(GitFileDiff.self, from: Data(page.utf8))
        })
        await model.load()
        XCTAssertEqual(model.hunks.map(\.index), [0, 1])
        XCTAssertEqual(model.progressText, "Showing 2 of 3 hunks")
        XCTAssertTrue(model.hasMore)
        await model.loadMore()
        XCTAssertEqual(model.hunks.map(\.index), [0, 1, 2])
        XCTAssertFalse(model.hasMore)
        XCTAssertNil(model.progressText)

        await model.load()
        staleOnce = true
        await model.loadMore()
        XCTAssertEqual(calls, [nil, "v1-ab-2", nil, "v1-ab-2", nil])
        XCTAssertEqual(model.hunks.map(\.index), [0, 1], "reloaded from the start, not mixed")
        XCTAssertNotNil(model.notice)
    }

    func testGitRefusalsArriveAsTheirSentenceAndCode() {
        let data = Data(#"{"error":"bare_repository","reason":"That is a bare repository. It has no working tree to show."}"#.utf8)
        let error = LatchGateway.error(status: 422, path: "/v2/git/resolve", data: data)
        XCTAssertEqual(error, .git(code: "bare_repository", reason: "That is a bare repository. It has no working tree to show."))
        XCTAssertEqual(error.message, "That is a bare repository. It has no working tree to show.")
        // A lost grant on a Git route is still a lost grant.
        XCTAssertEqual(LatchGateway.error(status: 403, path: "/v2/git/status", data: Data()), .unauthorized)
    }

    func testDiffQueryCarriesTheKeyAndAreaNotTheDisplayName() async throws {
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
            session: StubProtocol.session()
        )
        StubProtocol.stub(path: "/v2/git/diff", body: Self.diffPage(hunks: [0], total: 1, next: nil))
        _ = try await gateway.gitDiff(path: "/Users/person/src/my repo", fileKey: "2e2e2f", area: .staged, cursor: "v1-ab-1")
        let query = try XCTUnwrap(StubProtocol.requestQueries.last { $0.0 == "/v2/git/diff" }?.1)
        XCTAssertEqual(query, "path=/Users/person/src/my%20repo&file=2e2e2f&area=staged&cursor=v1-ab-1")
    }

    // MARK: Folder picker

    func testChooseRepositoryResolvesTheCurrentFolder() async throws {
        StubProtocol.stub(
            path: "/v2/directories",
            body: #"{"path":"/Users/person/src/latch/App","parent":"/Users/person/src/latch","entries":[],"nextCursor":null}"#
        )
        StubProtocol.stub(
            path: "/v2/git/resolve",
            body: #"{"root":"/Users/person/src/latch","name":"latch","kind":"repository"}"#
        )
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let opened = await model.newSessionFolderBrowser(mode: .chooseRepository)
        let browser = try XCTUnwrap(opened)
        XCTAssertFalse(browser.useCurrentAsDefault(), "choosing a repository never saves a default")
        await browser.openRepositoryHere()
        XCTAssertEqual(browser.chosenRepository?.root, "/Users/person/src/latch")
        XCTAssertFalse(StubProtocol.requests.contains { $0.method == "POST" })
    }

    // MARK: Helpers

    private func makePairedModel(
        authenticator: StubDeviceOwnerAuthenticator = StubDeviceOwnerAuthenticator(),
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

    private func pairedRecord(permission: DevicePermission) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: "mac", publicKey: String(repeating: "22", count: 32), name: "Mac"),
            permission: permission,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        )
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func increment() { lock.withLock { count += 1 } }
}
