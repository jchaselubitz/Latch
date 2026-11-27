import XCTest

@testable import LatchMobileKit

/// Slice 1 of Git management: read-only repository selection, status and
/// diffs. Every response here has the shape the Mac's gateway sends.
@MainActor
final class GitManagementTests: XCTestCase {
    private static func capabilities(
        git: Bool, scoped: Bool = false, summaries: Bool = false, sharedFiles: Bool = false, writes: Bool = false
    ) -> String {
        let gitKeys = git
            ? #","gitRepositories":true,"gitResolve":true,"gitStatus":true,"gitDiff":true"#
            : ""
        let scopedKey = scoped ? #","gitWorktreeReads":true"# : ""
        let summariesKey = summaries ? #","gitWorktreeSummaries":true"# : ""
        let filesKey = sharedFiles ? #","sharedFiles":true"# : ""
        let writesKey = writes ? #","gitWorktreeWrites":true,"gitReceipts":true,"gitCommit":true"# : ""
        return """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true\(gitKeys)\(scopedKey)\(summariesKey)\(filesKey)\(writesKey)},
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

    func testScopedReadsRequireDiscoveryAndCarryOnlyTheWorktreeReferenceAfterOpen() async throws {
        let link = try GatewayLink(address: "https://mac.local:8787", token: "token")
        let olderGateway = LatchGateway(link: link, session: StubProtocol.session())
        do {
            _ = try await olderGateway.openGitWorktree(path: "/Users/person/my repo")
            XCTFail("A gateway with only path-based Git must refuse a scoped read")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.gitWorktreeReads))
        }
        XCTAssertFalse(StubProtocol.requestQueries.contains { $0.0.hasPrefix("/v2/git/") })

        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true, scoped: true))
        let reference = String(repeating: "a", count: 64)
        StubProtocol.stub(path: "/v2/git/worktree/open", body: """
        {"worktreeRef":"\(reference)","repository":{"root":"/Users/person/my repo","name":"my repo","kind":"linked_worktree"}}
        """)
        StubProtocol.stub(path: "/v2/git/worktree/status", body: Self.status)
        StubProtocol.stub(path: "/v2/git/worktree/diff", body: Self.diffPage(hunks: [1], total: 2, next: nil))
        let gateway = LatchGateway(link: link, session: StubProtocol.session())
        let opened = try await gateway.openGitWorktree(path: "/Users/person/my repo")
        XCTAssertEqual(opened.repository.kind, .linkedWorktree)
        XCTAssertEqual(opened.worktreeRef, reference)
        _ = try await gateway.gitStatus(worktreeRef: opened.worktreeRef)
        _ = try await gateway.gitDiff(worktreeRef: opened.worktreeRef, fileKey: "77", area: .unstaged, cursor: "v1-ab-1")
        XCTAssertEqual(StubProtocol.requestQueries.last { $0.0 == "/v2/git/worktree/open" }?.1,
                       "path=/Users/person/my%20repo")
        XCTAssertEqual(StubProtocol.requestQueries.last { $0.0 == "/v2/git/worktree/status" }?.1,
                       "worktreeRef=\(reference)")
        XCTAssertEqual(StubProtocol.requestQueries.last { $0.0 == "/v2/git/worktree/diff" }?.1,
                       "worktreeRef=\(reference)&file=77&area=unstaged&cursor=v1-ab-1")
    }

    func testWorktreeSummariesNegotiateSeparatelyAndDecodeMixedResults() async throws {
        let link = try GatewayLink(address: "https://mac.local:8787", token: "token")
        let path = "/v2/git/worktree/summaries"
        let valid = String(repeating: "a", count: 64)
        let stale = String(repeating: "b", count: 64)
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true, scoped: true))
        let older = LatchGateway(link: link, session: StubProtocol.session())
        do {
            _ = try await older.gitWorktreeSummaries(worktreeRefs: [valid])
            XCTFail("Scoped reads alone do not advertise summaries")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.gitWorktreeSummaries))
        }
        XCTAssertFalse(StubProtocol.requestQueries.contains { $0.0 == path })

        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true, scoped: true, summaries: true))
        StubProtocol.stub(path: path, body: """
        {"results":[
          {"outcome":"error","error":"stale_worktree_ref"},
          {"outcome":"ok","summary":{"branch":"main","ahead":null,"behind":null,
            "staged":1,"unstaged":1,"conflicted":0,"untracked":999,"changesComplete":false}}
        ]}
        """)
        let gateway = LatchGateway(link: link, session: StubProtocol.session())
        let response = try await gateway.gitWorktreeSummaries(worktreeRefs: [stale, valid])
        XCTAssertEqual(response.results.count, 2)
        XCTAssertEqual(response.results.first, .error(.staleWorktreeRef))
        guard case .ok(let summary) = response.results.last else {
            return XCTFail("Expected a summary")
        }
        XCTAssertEqual(summary.branch, "main")
        XCTAssertNil(summary.ahead)
        XCTAssertNil(summary.behind)
        XCTAssertEqual(summary.untracked, 999)
        XCTAssertFalse(summary.changesComplete)
        let query = StubProtocol.requestQueries.last { $0.0 == path }?.1
        XCTAssertEqual(query?.removingPercentEncoding, "worktreeRefs=\(stale),\(valid)")
    }

    // MARK: Scoped reads

    func testScopedInteractionNegotiatesBeforePromptingAndKeepsSummariesOptional() async throws {
        let authenticator = StubDeviceOwnerAuthenticator(approves: false)
        let model = makePairedModel(authenticator: authenticator)
        let pairing = pairedRecord(permission: .control)
        await model.connectPairedDevice(pairing)
        await assertScopedError(.unsupported(.gitWorktreeReads)) { try await model.beginScopedGitRead() }
        XCTAssertEqual(authenticator.prompts, 0)

        stubScopedGit(summaries: false)
        await model.rediscover()
        XCTAssertTrue(model.advertisesScopedGitReads)
        XCTAssertFalse(model.advertisesScopedGitSummaries)
        XCTAssertTrue(model.applyPairedDeviceRecord(pairing.updating(permission: .observe)))
        await assertScopedError(.accessDenied) { try await model.beginScopedGitRead() }
        XCTAssertEqual(authenticator.prompts, 0)
        XCTAssertTrue(model.applyPairedDeviceRecord(pairing))
        await assertScopedError(.ownerCancelled) { try await model.beginScopedGitRead() }
        XCTAssertEqual(authenticator.prompts, 1)
        XCTAssertFalse(StubProtocol.requests.contains { $0.path.hasPrefix("/v2/git/") })

        authenticator.set(approves: true)
        let interaction = try await model.beginScopedGitRead()
        let worktree = try await interaction.open(path: Self.scopedRoot)
        await assertScopedError(.unsupported(.gitWorktreeSummaries)) {
            try await interaction.summaries(for: [worktree])
        }
        XCTAssertFalse(StubProtocol.requests.contains { $0.path == "/v2/git/worktree/summaries" })
    }

    func testScopedInteractionReadsExactRootsMixedSummariesStatusAndDiffPagesWithOnePrompt() async throws {
        stubScopedGit()
        let authenticator = StubDeviceOwnerAuthenticator()
        let model = makePairedModel(authenticator: authenticator)
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let interaction = try await model.beginScopedGitRead()
        let worktree = try await interaction.open(path: Self.scopedRoot)
        let secondReference = String(repeating: "b", count: 64)
        stubScopedOpen(reference: secondReference)
        let second = try await interaction.open(path: Self.scopedRoot)
        XCTAssertEqual(worktree.repository.kind, .linkedWorktree)
        await assertScopedError(.invalidSummaryCount) { try await interaction.summaries(for: []) }
        await assertScopedError(.invalidSummaryCount) {
            try await interaction.summaries(for: Array(repeating: worktree, count: 9))
        }
        let results = try await interaction.summaries(for: [worktree, second])
        XCTAssertEqual(results.first, .error(.staleWorktreeRef))
        guard case .ok(let summary) = results.last else { return XCTFail("Expected a summary") }
        XCTAssertNil(summary.ahead)
        XCTAssertFalse(summary.changesComplete)
        let status = try await interaction.status(for: second)
        XCTAssertEqual(status.head.branch, "git-ui")
        let diff = try await interaction.diff(for: second, fileKey: "77", area: .unstaged)
        _ = try await interaction.diff(for: second, fileKey: "77", area: .unstaged, cursor: diff.nextCursor)
        XCTAssertEqual(authenticator.prompts, 1)
        let requests = StubProtocol.requests.filter { $0.path.hasPrefix("/v2/git/") }
        XCTAssertTrue(requests.allSatisfy { $0.method == "GET" && $0.path.hasPrefix("/v2/git/worktree/") })
        XCTAssertEqual(requests.filter { $0.path.hasSuffix("/summaries") }.count, 1)
        XCTAssertEqual(requests.last?.query, "worktreeRef=\(secondReference)&file=77&area=unstaged&cursor=page-two")
    }

    func testScopedInteractionRejectsLateDiffAfterExplicitInvalidationOrGrantDowngrade() async throws {
        stubScopedGit()
        let model = makePairedModel()
        let pairing = pairedRecord(permission: .control)
        await model.connectPairedDevice(pairing)
        for losesGrant in [false, true] {
            let interaction = try await model.beginScopedGitRead()
            let worktree = try await interaction.open(path: Self.scopedRoot)
            let pending = try await holdScopedDiff(interaction, worktree)
            if losesGrant {
                XCTAssertTrue(model.applyPairedDeviceRecord(pairing.updating(permission: .interact)))
                XCTAssertTrue(model.applyPairedDeviceRecord(pairing))
            } else {
                interaction.invalidate()
            }
            XCTAssertFalse(interaction.isValid)
            pending.gate.release()
            await assertScopedError(.invalidated) { try await pending.reply.value }
        }
    }

    func testScopedInteractionRejectsLateDiffAfterReconnectAndAcrossSameNameHosts() async throws {
        stubScopedGit()
        let model = makePairedModel()
        let pairing = pairedRecord(permission: .control)
        await model.connectPairedDevice(pairing)
        let first = try await model.beginScopedGitRead()
        let worktree = try await first.open(path: Self.scopedRoot)
        let pending = try await holdScopedDiff(first, worktree)
        model.suspendPairedTransport()
        await assertScopedError(.transportUnavailable) { try await model.beginScopedGitRead() }
        await model.resumeAfterSuspension()
        pending.gate.release()
        await assertScopedError(.invalidated) { try await pending.reply.value }

        let reconnected = try await model.beginScopedGitRead()
        let reconnectedWorktree = try await reconnected.open(path: Self.scopedRoot)
        let switched = try await holdScopedDiff(reconnected, reconnectedWorktree)
        let otherHost = pairedRecord(permission: .control, macID: "mac-2", macKeyByte: "33")
        XCTAssertEqual(otherHost.mac.displayName, pairing.mac.displayName)
        await model.connectPairedDevice(otherHost)
        switched.gate.release()
        await assertScopedError(.invalidated) { try await switched.reply.value }
        let second = try await model.beginScopedGitRead()
        let readsBefore = StubProtocol.requests.count
        await assertScopedError(.invalidWorktree) { try await second.status(for: worktree) }
        XCTAssertEqual(StubProtocol.requests.count, readsBefore)

        stubScopedOpen(reference: String(repeating: "c", count: 64))
        StubProtocol.stub(path: "/v2/git/worktree/status", body: Self.status.replacingOccurrences(of: "git-ui", with: "other-host"))
        let otherWorktree = try await second.open(path: Self.scopedRoot)
        let otherStatus = try await second.status(for: otherWorktree)
        XCTAssertEqual(otherStatus.head.branch, "other-host")
        XCTAssertEqual(otherWorktree.repository.root, worktree.repository.root)
        XCTAssertEqual(StubProtocol.requests.last?.headers["Authorization"], "Bearer mac-2")
    }

    func testScopedErrorsDistinguishStaleCursorAndWorktreeFromHostDenial() async throws {
        stubScopedGit()
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let interaction = try await model.beginScopedGitRead()
        let other = try await model.beginScopedGitRead()
        let worktree = try await interaction.open(path: Self.scopedRoot)
        StubProtocol.stub(path: "/v2/git/worktree/diff", status: 409, body: #"{"error":"stale_cursor"}"#)
        await assertScopedError(.staleCursor) { try await interaction.diff(for: worktree, fileKey: "77", area: .unstaged) }
        let path = "/v2/git/worktree/status"
        StubProtocol.stub(path: path, status: 409, body: #"{"error":"stale_worktree_ref"}"#)
        await assertScopedError(.staleWorktree) { try await interaction.status(for: worktree) }
        XCTAssertTrue(interaction.isValid)
        let otherWorktree = try await other.open(path: Self.scopedRoot)
        StubProtocol.stub(path: "/v2/git/worktree/diff", body: Self.diffPage(hunks: [0], total: 1, next: nil))
        let pending = try await holdScopedDiff(other, otherWorktree)
        StubProtocol.stub(path: path, status: 403, body: #"{"error":"worktree_access_denied"}"#)
        await assertScopedError(.accessDenied) { try await interaction.status(for: worktree) }
        XCTAssertFalse(interaction.isValid)
        XCTAssertFalse(other.isValid)
        pending.gate.release()
        await assertScopedError(.invalidated) { try await pending.reply.value }
    }

    func testScopedOwnerAuthorizationExpiresEvenIfAnotherInteractionRenewsIt() async throws {
        stubScopedGit()
        let clock = MutableClock()
        let authenticator = StubDeviceOwnerAuthenticator()
        let model = makePairedModel(authenticator: authenticator, ownerClock: { clock.now })
        await model.connectPairedDevice(pairedRecord(permission: .control))
        let old = try await model.beginScopedGitRead()
        let worktree = try await old.open(path: Self.scopedRoot)
        clock.advance(601)
        let current = try await model.beginScopedGitRead()
        XCTAssertTrue(current.isValid)
        await assertScopedError(.ownerAuthorizationExpired) { try await old.status(for: worktree) }
        XCTAssertFalse(old.isValid)
        XCTAssertEqual(authenticator.prompts, 2)
    }

    func testScopedGitWritePersistsBeforeSendingAndRecoversAfterColdLaunch() async throws {
        stubScopedGit()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true, scoped: true, writes: true))
        let folder = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appending(path: "writes.json")
        let pairing = pairedRecord(permission: .control)
        let app = makePairedModel()
        await app.connectPairedDevice(pairing)
        XCTAssertTrue(app.advertisesScopedGitWrites)
        let first = try await app.beginScopedGitWrite(store: PendingGitWriteStore(file: file))
        let worktree = try await first.worktrees.open(path: Self.scopedRoot)
        let expected = GitPreconditions(branch: "main", head: nil, index: "reviewed index", worktree: "reviewed worktree")
        let requestID = UUID()
        let action = GitMutationAction.commit(GitCommitRequest(
            requestId: requestID.uuidString, path: Self.scopedRoot, expected: expected,
            summary: "private commit message", description: nil
        ))
        // The mutation response is lost after the request crossed the adapter.
        let route = "/v2/git/commit"
        StubProtocol.stub(path: route, status: 502, body: #"{"error":"receipt_unavailable","message":"Lost response"}"#)
        let gate = StubProtocol.holdNextResponse(path: route)
        defer { gate.release() }
        let response = Task { try await first.submit(action, for: worktree) }
        for _ in 0..<100 {
            if StubProtocol.requests.contains(where: { $0.path == route }) { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        let written = try String(contentsOf: file, encoding: .utf8)
        let reference = String(repeating: "a", count: 64)
        XCTAssertTrue(written.contains(requestID.uuidString))
        XCTAssertTrue(written.contains(reference))
        XCTAssertFalse(written.contains("private commit message"))
        XCTAssertFalse(written.contains("reviewed index"))
        XCTAssertTrue(StubProtocol.requests.last?.body.contains("worktree:\(reference)") == true)
        XCTAssertTrue(StubProtocol.requests.last?.body.contains("reviewed index") == true)
        gate.release()
        do {
            _ = try await response.value
            XCTFail("Expected a lost response")
        } catch {}

        let cold = makePairedModel()
        await cold.connectPairedDevice(pairing)
        let recovery = try await cold.beginScopedGitWrite(store: PendingGitWriteStore(file: file))
        let entries = try await recovery.pendingWrites()
        let pending = try XCTUnwrap(entries.first)
        XCTAssertEqual(pending.requestID, requestID)
        XCTAssertEqual(pending.operation, .commit)

        let otherApp = makePairedModel()
        await otherApp.connectPairedDevice(pairedRecord(permission: .control, macID: "mac-2", macKeyByte: "33"))
        let otherScope = try await otherApp.beginScopedGitWrite(store: PendingGitWriteStore(file: file))
        let otherWrites = try await otherScope.pendingWrites()
        XCTAssertTrue(otherWrites.isEmpty)
        do {
            _ = try await otherScope.recover(pending)
            XCTFail("A different authenticated Mac cannot recover this write")
        } catch {
            XCTAssertEqual(error as? ScopedGitWriteError, .unknownPendingWrite)
        }

        let receiptRoute = "/v2/git/receipts/\(requestID.uuidString)"
        StubProtocol.stub(path: receiptRoute, status: 404, body: #"{"error":"receipt_not_found","message":"Unknown"}"#)
        let missing = try await recovery.recover(pending)
        XCTAssertNil(missing)
        let stillPending = try await recovery.pendingWrites()
        XCTAssertEqual(stillPending, [pending])
        for state in ["pending", "uncertain", "completed"] {
            StubProtocol.stub(path: receiptRoute, body: """
            {"requestId":"\(requestID.uuidString)","root":"\(Self.scopedRoot)","operation":"commit",
             "state":"\(state)","acceptedAt":"2026-11-27T12:00:00Z","finishedAt":null,"outcome":null}
            """)
            let receipt = try await recovery.recover(pending)
            XCTAssertEqual(receipt?.state.rawValue, state)
            let acknowledged = try await recovery.acknowledge(pending)
            XCTAssertEqual(acknowledged, state == "completed")
            XCTAssertEqual(StubProtocol.requests.last?.query, "worktreeRef=\(reference)")
        }
        let finished = try await recovery.pendingWrites()
        XCTAssertTrue(finished.isEmpty)
        XCTAssertEqual(StubProtocol.requests.filter { $0.method == "POST" && $0.path == route }.count, 1)
    }

    private static let scopedRoot = "/Users/person/src/latch"

    private func stubScopedGit(summaries: Bool = true) {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: true, scoped: true, summaries: summaries))
        stubScopedOpen(reference: String(repeating: "a", count: 64))
        StubProtocol.stub(path: "/v2/git/worktree/status", body: Self.status)
        StubProtocol.stub(path: "/v2/git/worktree/diff", body: Self.diffPage(hunks: [0], total: 2, next: "page-two"))
        StubProtocol.stub(path: "/v2/git/worktree/summaries", body: """
        {"results":[{"outcome":"error","error":"stale_worktree_ref"},
          {"outcome":"ok","summary":{"branch":"main","ahead":null,"behind":null,
           "staged":1,"unstaged":1,"conflicted":0,"untracked":999,"changesComplete":false}}]}
        """)
    }

    private func stubScopedOpen(reference: String) {
        StubProtocol.stub(path: "/v2/git/worktree/open", body: """
        {"worktreeRef":"\(reference)","repository":{"root":"\(Self.scopedRoot)","name":"latch","kind":"linked_worktree"}}
        """)
    }

    private func assertScopedError<Value>(
        _ expected: ScopedGitReadError, file: StaticString = #filePath, line: UInt = #line,
        operation: () async throws -> Value
    ) async {
        do {
            _ = try await operation()
            XCTFail("Expected \(expected)", file: file, line: line)
        } catch {
            XCTAssertEqual(error as? ScopedGitReadError, expected, file: file, line: line)
        }
    }

    private func holdScopedDiff(
        _ interaction: ScopedGitReadInteraction, _ worktree: ScopedGitWorktree
    ) async throws -> (gate: StubProtocol.ResponseGate, reply: Task<GitFileDiff, Error>) {
        let path = "/v2/git/worktree/diff"
        let count = StubProtocol.requests.filter { $0.path == path }.count
        let gate = StubProtocol.holdNextResponse(path: path)
        let reply = Task { try await interaction.diff(for: worktree, fileKey: "77", area: .unstaged) }
        for _ in 0..<100 {
            if StubProtocol.requests.filter({ $0.path == path }).count > count { return (gate, reply) }
            try await Task.sleep(for: .milliseconds(10))
        }
        gate.release()
        reply.cancel()
        throw ScopedGitReadError.transportUnavailable
    }

    func testSharedFilesNegotiationPreviewOutcomesAndStaleCursor() async throws {
        let authenticator = StubDeviceOwnerAuthenticator()
        let model = makePairedModel(authenticator: authenticator)
        await model.connectPairedDevice(pairedRecord(permission: .control))
        do {
            _ = try await model.beginSharedFilesRead()
            XCTFail("Expected older gateway to be unavailable")
        } catch {
            XCTAssertEqual(error as? SharedFilesReadError, .unsupported)
        }
        XCTAssertEqual(authenticator.prompts, 0)
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: false, sharedFiles: true))
        await model.rediscover()
        let rootID = String(repeating: "a", count: 64)
        StubProtocol.stub(path: "/v2/files/roots", body: """
        {"roots":[{"rootId":"\(rootID)","name":"Project"}]}
        """)
        let interaction = try await model.beginSharedFilesRead()
        let roots = try await interaction.roots()
        XCTAssertEqual(roots.first?.id, rootID)
        let preview = "/v2/files/preview"
        StubProtocol.stub(path: preview, body: #"{"kind":"truncated","text":"hello"}"#)
        let truncated = try await interaction.preview(rootID: rootID, path: "dir/a+b.txt")
        XCTAssertEqual(truncated, .truncated("hello"))
        XCTAssertTrue(StubProtocol.requestQueries.last?.1?.contains("a%2Bb.txt") == true)
        StubProtocol.stub(path: preview, body: #"{"kind":"binary"}"#)
        let binary = try await interaction.preview(rootID: rootID, path: "image")
        XCTAssertEqual(binary, .binary)
        let directory = "/v2/files/directory"
        StubProtocol.stub(path: directory, status: 409, body: #"{"error":"stale_cursor"}"#)
        do {
            _ = try await interaction.directory(rootID: rootID, cursor: "opaque")
            XCTFail("Expected stale cursor")
        } catch {
            XCTAssertEqual(error as? SharedFilesReadError, .staleCursor)
        }
        XCTAssertTrue(interaction.isValid)
        XCTAssertEqual(authenticator.prompts, 1)
    }

    func testSharedFilesDiscardsPendingPreviewOnGrantDowngrade() async throws {
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(git: false, sharedFiles: true))
        let model = makePairedModel()
        let pairing = pairedRecord(permission: .control)
        await model.connectPairedDevice(pairing)
        let interaction = try await model.beginSharedFilesRead()
        let path = "/v2/files/preview"
        StubProtocol.stub(path: path, body: #"{"kind":"text","text":"Late file content"}"#)
        let gate = StubProtocol.holdNextResponse(path: path)
        let reply = Task { try await interaction.preview(rootID: "root", path: "README.md") }
        for _ in 0..<100 {
            if StubProtocol.requests.contains(where: { $0.path == path }) { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(StubProtocol.requests.contains { $0.path == path })
        XCTAssertTrue(model.applyPairedDeviceRecord(pairing.updating(permission: .observe)))
        gate.release()
        do {
            _ = try await reply.value
            XCTFail("Expected preview to be discarded")
        } catch {
            XCTAssertEqual(error as? SharedFilesReadError, .invalidated)
        }
        XCTAssertFalse(interaction.isValid)
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
        terminalConnector: AppModel.TerminalConnecting? = nil,
        ownerClock: @escaping @Sendable () -> Date = { Date() }
    ) -> AppModel {
        AppModel(
            pairedGatewayFactory: { record in
                LatchGateway(
                    link: try GatewayLink(address: "https://mac.local:8787", token: record.mac.deviceId),
                    session: StubProtocol.session()
                )
            },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalConnector: terminalConnector,
            terminalUnlock: TerminalUnlock(authenticator: authenticator, grace: 600, now: ownerClock)
        )
    }

    private func pairedRecord(
        permission: DevicePermission, macID: String = "mac", macKeyByte: String = "22"
    ) -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: macID, publicKey: String(repeating: macKeyByte, count: 32), name: "Mac"),
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
