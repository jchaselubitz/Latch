import XCTest

@testable import LatchMobileKit

/// Slice 3 of Git management: the Sync sheet, fetch, fast-forward, merge
/// and rebase, Continue/Abort, progress, and receipt reconciliation for
/// changes that may run for minutes. Every response has the gateway's shape.
@MainActor
final class GitSyncTests: XCTestCase {
    nonisolated static let head = GitMutationTests.head
    nonisolated static let upstreamOid = String(repeating: "e", count: 40)
    nonisolated static let index = GitMutationTests.index
    nonisolated static let worktree = GitMutationTests.worktree
    nonisolated static let expected = GitMutationTests.expected
    nonisolated static let preconditions = #"{"branch":"main","head":"\#(head)","index":"\#(index)","worktree":"\#(worktree)"}"#

    nonisolated static func status(operation: String = "null", conflicted: Bool = false) -> String {
        let conflict = conflicted ? """
          {"path":"both.txt","pathKey":"64","origPath":null,"origPathKey":null,"area":"conflicted",
           "change":"conflicted","conflict":"UU","submodule":false,"additions":null,"deletions":null,"binary":false,
           "fingerprint":"4444444444444444"},
        """ : ""
        return """
        {"root":"/r","name":"r","kind":"repository",
         "head":{"branch":"main","oid":"\(head)","detached":false},
         "upstream":{"name":"origin/main","ahead":1,"behind":2},"lastFetchedAt":"2026-10-13T10:00:00Z",
         "operation":\(operation),
         "entries":[\(conflict)
          {"path":"README.md","pathKey":"62","origPath":null,"origPathKey":null,"area":"unstaged",
           "change":"modified","conflict":null,"submodule":false,"additions":1,"deletions":0,"binary":false,
           "fingerprint":"2222222222222222"}
         ],
         "entriesTruncated":false,"totalEntries":2,"sessions":[],
         "observedAt":"2026-10-13T10:04:00Z","preconditions":\(preconditions)}
        """
    }

    nonisolated static func preview(
        relation: String = "diverged", blocked: String = #""main has 1 commit of its own, so it can't be fast-forwarded.""#,
        incoming: [String] = ["f1", "f2"], cursor: String = "null", upstreamOid: String? = upstreamOid,
        operation: String = "null", remote: String = #""origin""#
    ) -> String {
        let commits = incoming.map { oid in
            #"{"oid":"\#(oid)","subject":"Commit \#(oid)","author":"Ana","committedAt":"2026-10-13T09:00:00Z"}"#
        }.joined(separator: ",")
        let oid = upstreamOid.map { "\"\($0)\"" } ?? "null"
        return """
        {"root":"/r","head":{"branch":"main","oid":"\(head)","detached":false},
         "upstream":{"name":"origin/main","ref":"refs/remotes/origin/main","remote":\(remote),
          "remoteUrl":"https://github.com/acme/r.git","oid":\(oid),"gone":false},
         "remotes":[{"name":"origin","url":"https://github.com/acme/r.git"},{"name":"fork","url":null}],
         "lastFetchedAt":"2026-10-13T10:00:00Z","relation":"\(relation)","ahead":1,"behind":2,
         "fastForwardBlockedReason":\(blocked),"incoming":[\(commits)],"nextCursor":\(cursor),
         "operation":\(operation),"preconditions":\(preconditions)}
        """
    }

    nonisolated static func receipt(
        _ id: String, state: String, operation: String, code: String? = nil, message: String = "Done.",
        files: String = "[]", progress: String = "null", startedAt: String = #""2026-10-13T10:05:01Z""#
    ) -> String {
        let outcome = code.map {
            #"{"code":"\#($0)","message":"\#(message)","gitOutput":"git said no","gitOutputTruncated":false,"head":null,"branch":"main","commit":null,"files":\#(files),"reconciled":false}"#
        } ?? "null"
        return """
        {"requestId":"\(id)","operation":"\(operation)","root":"/r","state":"\(state)",
         "acceptedAt":"2026-10-13T10:05:00Z","finishedAt":null,"outcome":\(outcome),
         "startedAt":\(startedAt),"progress":\(progress)}
        """
    }

    nonisolated static func progress(_ message: String, _ percent: Int) -> String {
        #"{"message":"\#(message)","percent":\#(percent),"updatedAt":"2026-10-13T10:05:30Z"}"#
    }

    // MARK: Decoding

    func testSyncPreviewDecodesIncludingUnknownRelationsAndMissingFields() throws {
        let preview = try JSONDecoder().decode(GitSyncPreview.self, from: Data(Self.preview(cursor: #""c2""#).utf8))
        XCTAssertEqual(preview.relation, .diverged)
        XCTAssertEqual(preview.upstream?.remote, "origin")
        XCTAssertEqual(preview.upstream?.oid, Self.upstreamOid)
        XCTAssertEqual(preview.remotes.map(\.name), ["origin", "fork"])
        XCTAssertEqual(preview.incoming.map(\.oid), ["f1", "f2"])
        XCTAssertEqual(preview.nextCursor, "c2")
        XCTAssertEqual(preview.preconditions, Self.expected)
        XCTAssertEqual(preview.refsLine, "origin/main \u{2192} main")
        XCTAssertEqual(preview.outgoingLine, "\u{2191}1 to push")
        XCTAssertNotNil(preview.fastForwardBlockedReason)

        let future = try JSONDecoder().decode(
            GitSyncPreview.self, from: Data(Self.preview(relation: "sideways").utf8)
        )
        XCTAssertEqual(future.relation, .unknown)
        XCTAssertNotNil(future.relationExplanation, "an unknown relation offers no update")

        // Every optional field left out entirely: no upstream, never fetched.
        let sparse = """
        {"root":"/r","head":{"branch":"main","oid":null,"detached":false},"remotes":[],
         "relation":"no_upstream","incoming":[],"preconditions":\(Self.preconditions)}
        """
        let minimal = try JSONDecoder().decode(GitSyncPreview.self, from: Data(sparse.utf8))
        XCTAssertNil(minimal.upstream)
        XCTAssertNil(minimal.lastFetchedAt)
        XCTAssertNil(minimal.ahead)
        XCTAssertNil(minimal.fastForwardBlockedReason)
        XCTAssertNil(minimal.nextCursor)
        XCTAssertNil(minimal.operation)
        XCTAssertEqual(minimal.relation, .noUpstream)
    }

    func testStatusOperationFromOlderAndNewerMacs() throws {
        let older = try JSONDecoder().decode(
            GitStatus.self, from: Data(Self.status(operation: #"{"kind":"rebase","step":3,"total":5}"#).utf8)
        )
        let operation = try XCTUnwrap(older.operation)
        XCTAssertEqual(operation.kind, .rebase)
        XCTAssertNil(operation.headName)
        XCTAssertNil(operation.onto)
        XCTAssertNil(operation.mergeHead)
        let state = try XCTUnwrap(GitOperationState(status: older))
        XCTAssertEqual(state.stepLine, "Stopped at commit 3 of 5")
        XCTAssertEqual(state.refs(branch: "main"), "Rebasing main")

        let newer = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status(
            operation: #"{"kind":"rebase","step":3,"total":5,"headName":"refs/heads/topic","onto":"\#(Self.upstreamOid)","stoppedAt":"\#(Self.head)"}"#,
            conflicted: true
        ).utf8))
        let rebase = try XCTUnwrap(GitOperationState(status: newer))
        XCTAssertEqual(rebase.refs(branch: nil), "Rebasing topic onto eeeeeee")
        XCTAssertEqual(rebase.conflictLine, "1 conflicted file")
        XCTAssertFalse(rebase.canContinue)
        XCTAssertEqual(rebase.continueLabel, "Continue rebase")

        let merge = GitOperationState(
            operation: GitOperation(kind: .merge, mergeHead: Self.upstreamOid, summary: "Merge origin/main"),
            conflictedCount: 0
        )
        XCTAssertEqual(merge.refs(branch: "main"), "Merging Merge origin/main into main")
        XCTAssertTrue(merge.canContinue)
        XCTAssertNil(merge.continueBlockedReason)
        XCTAssertEqual(merge.abortLabel, "Abort merge")
    }

    func testReceiptCarriesProgressStartedAtAndTheNewKinds() throws {
        let pending = try GitMutationTests.decodeReceipt(Self.receipt(
            "id", state: "pending", operation: "fetch", progress: Self.progress("Receiving objects", 45)
        ))
        XCTAssertEqual(pending.operation, .fetch)
        XCTAssertEqual(pending.progress?.message, "Receiving objects")
        XCTAssertEqual(pending.progress?.percent, 45)
        XCTAssertEqual(pending.startedAt, "2026-10-13T10:05:01Z")
        let kinds = ["fast_forward": GitMutationKind.fastForward, "merge": .merge, "rebase": .rebase,
                     "continue_operation": .continueOperation, "abort_operation": .abortOperation]
        for (wire, kind) in kinds {
            let receipt = try GitMutationTests.decodeReceipt(Self.receipt("id", state: "completed", operation: wire, code: "merged"))
            XCTAssertEqual(receipt.operation, kind, wire)
            XCTAssertTrue(kind.isLongRunning)
        }
        // A slice-2 receipt has neither field.
        let older = try GitMutationTests.decodeReceipt(GitMutationTests.receipt("id", state: "completed"))
        XCTAssertNil(older.startedAt)
        XCTAssertNil(older.progress)
    }

    // MARK: Gating

    func testEachSlice3ActionIsGatedOnItsOwnKeyAndOnReceipts() throws {
        func support(_ keys: String) throws -> GitMutationSupport {
            GitMutationSupport(endpoints: try JSONDecoder().decode(
                GatewayEndpoints.self,
                from: Data(#"{"sessions":true,"terminal":true,"conversation":true\#(keys)}"#.utf8)
            ))
        }
        XCTAssertEqual(try support(#","gitSync":true,"gitFetch":true,"gitMerge":true"#), .none,
                       "without receipts nothing is offered")
        let some = try support(#","gitReceipts":true,"gitSync":true,"gitFetch":true,"gitMerge":true,"gitStageResolved":true"#)
        XCTAssertTrue(some.sync)
        XCTAssertTrue(some.fetch)
        XCTAssertTrue(some.merge)
        XCTAssertFalse(some.fastForward)
        XCTAssertFalse(some.rebase)
        XCTAssertFalse(some.continueOperation)
        XCTAssertFalse(some.abortOperation)
        XCTAssertFalse(some.stageResolved, "marking resolved goes through the stage route")
        let all = try support(GitMutationTests.allKeys + Self.slice3Keys)
        XCTAssertEqual(all, .everything)
    }

    func testTheSyncSheetOnlyOffersAdvertisedModes() async throws {
        var mergeOnly = GitMutationSupport.all
        mergeOnly.sync = true
        mergeOnly.merge = true
        let harness = SyncHarness(support: mergeOnly)
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        XCTAssertEqual(sync.availableModes, [.merge])
        XCTAssertEqual(sync.mode, .merge, "the default is fast-forward only when the Mac serves it")
        XCTAssertFalse(harness.model.offersFetch)
        XCTAssertFalse(harness.model.offersContinue)

        let none = SyncHarness(support: .all)
        await none.model.refresh()
        XCTAssertNil(none.model.syncModel(), "no Sync sheet without gitSync and an update route")
    }

    func testFastForwardIsTheDefaultAndDisabledWithTheMacsReasonWhenDiverged() async throws {
        let harness = SyncHarness()
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        XCTAssertEqual(sync.mode, .fastForward)
        XCTAssertEqual(sync.modeBlockedReason(.fastForward), "main has 1 commit of its own, so it can't be fast-forwarded.")
        XCTAssertFalse(sync.isModeEnabled(.fastForward))
        XCTAssertEqual(sync.updateBlockReason, sync.modeBlockedReason(.fastForward))
        await sync.update()
        XCTAssertTrue(harness.sent.isEmpty, "a blocked fast-forward is never sent")

        sync.mode = .merge
        XCTAssertTrue(sync.canUpdate)
        XCTAssertEqual(sync.primaryTitle, "Merge origin/main into main")
        sync.mode = .rebase
        XCTAssertEqual(sync.primaryTitle, "Rebase main onto origin/main")
        XCTAssertEqual(GitSyncMode.fastForward.buttonTitle(upstream: "origin/main", branch: "main"), "Fast-forward main")

        harness.previewJSON = Self.preview(relation: "behind", blocked: "null")
        await sync.load()
        sync.mode = .fastForward
        XCTAssertTrue(sync.canUpdate)
    }

    func testNothingIsBroughtInWhenUpToDateOrWhileAnOperationRuns() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(relation: "up_to_date", blocked: "null", incoming: [])
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        XCTAssertTrue(sync.updateBlockReason?.contains("up to date") == true)
        harness.previewJSON = Self.preview(relation: "behind", blocked: "null", operation: #"{"kind":"merge"}"#)
        await sync.load()
        XCTAssertEqual(sync.updateBlockReason, "Finish the merge or rebase in progress first.")
    }

    // MARK: Paging

    func testLoadMoreAppendsAndAStaleCursorRestartsAtPageOne() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(incoming: ["a", "b"], cursor: #""c2""#)
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        XCTAssertTrue(sync.hasMore)
        harness.pages["c2"] = .success(Self.preview(incoming: ["c"], cursor: "null"))
        await sync.loadMore()
        XCTAssertEqual(sync.incoming.map(\.oid), ["a", "b", "c"])
        XCTAssertFalse(sync.hasMore)

        harness.previewJSON = Self.preview(incoming: ["x", "y"], cursor: #""c9""#)
        await sync.load()
        harness.pages["c9"] = .failure(LatchError.git(code: "stale_cursor", reason: "The upstream moved."))
        harness.previewJSON = Self.preview(incoming: ["z", "x", "y"], cursor: "null")
        await sync.loadMore()
        XCTAssertEqual(sync.incoming.map(\.oid), ["z", "x", "y"], "a moved upstream is read again from the first page")
        XCTAssertEqual(harness.syncCursors.suffix(2), ["c9", nil])
        XCTAssertNotNil(sync.notice)
        XCTAssertNil(sync.error)
    }

    // MARK: Requests

    func testUpdateRequestsCarryThePreviewExactly() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(relation: "behind", blocked: "null")
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        for mode in GitSyncMode.allCases {
            harness.replies.append(.receipt(Self.receipt("x", state: "completed", operation: "merge", code: "merged")))
            sync.mode = mode
            await sync.update()
            harness.model.mutations.acknowledge()
        }
        XCTAssertEqual(harness.sent.count, 3)
        let requests: [GitUpdateRequest] = harness.sent.compactMap {
            switch $0 {
            case .fastForward(let request), .merge(let request), .rebase(let request): request
            default: nil
            }
        }
        XCTAssertEqual(requests.count, 3)
        guard case .fastForward = harness.sent[0], case .merge = harness.sent[1], case .rebase = harness.sent[2] else {
            return XCTFail("\(harness.sent)")
        }
        for request in requests {
            XCTAssertEqual(request.upstream, "origin/main")
            XCTAssertEqual(request.upstreamOid, Self.upstreamOid)
            XCTAssertEqual(request.expected, Self.expected)
            XCTAssertEqual(request.path, "/r")
        }
        XCTAssertEqual(Set(requests.map(\.requestId)).count, 3, "every new choice is a new request")
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(requests[0])) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["requestId", "path", "expected", "upstream", "upstreamOid"])
        XCTAssertGreaterThanOrEqual(harness.previewReads, 4, "the preview is read again after every outcome")
    }

    func testContinueWaitsForConflictsAndAbortCarriesConfirmation() async throws {
        let harness = SyncHarness(statusJSON: Self.status(
            operation: #"{"kind":"merge","mergeHead":"\#(Self.upstreamOid)","summary":"origin/main"}"#, conflicted: true
        ))
        await harness.model.refresh()
        XCTAssertTrue(harness.model.offersContinue)
        XCTAssertFalse(harness.model.canContinue)
        XCTAssertEqual(harness.model.continueBlockReason, "Stage the conflicted file once it's resolved, then continue.")
        await harness.model.continueOperation()
        XCTAssertTrue(harness.sent.isEmpty)
        XCTAssertEqual(harness.model.syncBlockReason, "Finish the merge or rebase in progress first.")
        XCTAssertFalse(harness.model.canCommit)

        harness.replies = [.receipt(Self.receipt("x", state: "completed", operation: "abort_operation", code: "aborted", message: "Aborted the merge."))]
        XCTAssertTrue(harness.model.canAbort)
        await harness.model.abortOperation()
        guard case .abortOperation(let abort) = try XCTUnwrap(harness.sent.first) else { return XCTFail() }
        XCTAssertTrue(abort.confirmed)
        XCTAssertEqual(abort.operation, .merge)
        XCTAssertEqual(abort.expected, Self.expected)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(abort)) as? [String: Any])
        XCTAssertEqual(object["confirmed"] as? Bool, true)
        XCTAssertEqual(object["operation"] as? String, "merge")

        let resolved = SyncHarness(statusJSON: Self.status(operation: #"{"kind":"rebase","step":2,"total":4}"#))
        await resolved.model.refresh()
        XCTAssertTrue(resolved.model.canContinue, "a rebase stop without conflicts can continue")
        resolved.replies = [.receipt(Self.receipt(
            "x", state: "rejected", operation: "continue_operation", code: "unresolved_conflicts",
            message: "1 file still has conflicts.", files: #"["both.txt"]"#
        ))]
        await resolved.model.continueOperation()
        guard case .continueOperation(let request) = try XCTUnwrap(resolved.sent.first) else { return XCTFail() }
        XCTAssertFalse(request.confirmed)
        XCTAssertEqual(request.operation, .rebase)
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(resolved.model.mutations.phase))
        XCTAssertEqual(presentation.style, .rejected)
        XCTAssertEqual(presentation.files, ["both.txt"])
        XCTAssertEqual(presentation.gitOutput, "git said no")
    }

    func testConflictedFilesAreStagedOnlyWhenTheMacOffersIt() async throws {
        let older = SyncHarness(support: .all, statusJSON: Self.status(operation: #"{"kind":"merge"}"#, conflicted: true))
        await older.model.refresh()
        let conflicted = try XCTUnwrap(older.model.status?.entries.first { $0.area == .conflicted })
        XCTAssertFalse(older.model.offersStaging(conflicted))
        await older.model.toggleStaging(conflicted)
        XCTAssertTrue(older.sent.isEmpty)

        let newer = SyncHarness(statusJSON: Self.status(operation: #"{"kind":"merge"}"#, conflicted: true))
        await newer.model.refresh()
        XCTAssertTrue(newer.model.offersStaging(conflicted))
        newer.replies = [.receipt(Self.receipt("x", state: "completed", operation: "stage", code: "staged"))]
        await newer.model.toggleStaging(conflicted)
        guard case .stage(let request) = try XCTUnwrap(newer.sent.first) else { return XCTFail() }
        XCTAssertEqual(request.files.first?.area, .conflicted)
        XCTAssertFalse(request.confirmConflictMarkers)
        let json = try XCTUnwrap(String(data: JSONEncoder().encode(request), encoding: .utf8))
        XCTAssertFalse(json.contains("confirmConflictMarkers"), "the flag is only sent when true")
        XCTAssertTrue(GitMutationAction.stage(request).stagesConflictedFiles)
        var confirmed = request
        confirmed.confirmConflictMarkers = true
        let confirmedJSON = try XCTUnwrap(String(data: JSONEncoder().encode(confirmed), encoding: .utf8))
        XCTAssertTrue(confirmedJSON.contains(#""confirmConflictMarkers":true"#))
    }

    func testConflictMarkersAskThenSendANewConfirmedRequest() async throws {
        let harness = SyncHarness(statusJSON: Self.status(operation: #"{"kind":"merge"}"#, conflicted: true))
        await harness.model.refresh()
        let conflicted = try XCTUnwrap(harness.model.status?.entries.first { $0.area == .conflicted })
        harness.replies = [
            .receipt(Self.receipt("x", state: "rejected", operation: "stage", code: "conflict_markers",
                                  message: "both.txt still has markers.", files: #"["both.txt"]"#)),
            .receipt(Self.receipt("y", state: "completed", operation: "stage", code: "staged")),
        ]
        await harness.model.toggleStaging(conflicted)
        XCTAssertEqual(harness.model.conflictMarkersQuestion, "both.txt still contains conflict markers. Stage it anyway?")
        await harness.model.confirmConflictMarkers()
        XCTAssertEqual(harness.sent.count, 2)
        guard case .stage(let first) = harness.sent[0], case .stage(let second) = harness.sent[1] else { return XCTFail() }
        XCTAssertNotEqual(first.requestId, second.requestId, "the refused request ran nothing; this is a new one")
        XCTAssertFalse(first.confirmConflictMarkers)
        XCTAssertTrue(second.confirmConflictMarkers)
        XCTAssertEqual(second.files, first.files)
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertNil(harness.model.conflictMarkersQuestion)

        // Declining clears the question and sends nothing.
        harness.replies = [.receipt(Self.receipt("z", state: "rejected", operation: "stage", code: "conflict_markers", files: #"["both.txt"]"#))]
        harness.model.mutations.acknowledge()
        await harness.model.toggleStaging(conflicted)
        XCTAssertNotNil(harness.model.conflictMarkersQuestion)
        harness.model.declineConflictMarkers()
        XCTAssertNil(harness.model.conflictMarkersQuestion)
        XCTAssertEqual(harness.sent.count, 3)
    }

    // MARK: Progress and reconciliation

    func testAFetchIsReadUntilItSettlesWellPastThirtySeconds() async throws {
        let harness = SyncHarness()
        await harness.model.refresh()
        let pendingReads = 90
        harness.replies = [.receipt(Self.receipt("x", state: "pending", operation: "fetch", progress: Self.progress("Counting objects", 1)))]
        harness.lookups = (0..<pendingReads).map { step in
            .receipt(Self.receipt("x", state: "pending", operation: "fetch", progress: Self.progress("Receiving objects", step)))
        } + [.receipt(Self.receipt("x", state: "completed", operation: "fetch", code: "fetched",
                                    message: "Fetched origin · 1 new commit on origin/main"))]
        var sawProgress: GitProgressPresentation?
        harness.onSleep = { _ in
            if sawProgress == nil { sawProgress = harness.model.progress }
        }
        await harness.model.fetch(remote: "origin")
        guard case .fetch(let request) = try XCTUnwrap(harness.sent.first) else { return XCTFail() }
        XCTAssertEqual(request.remote, "origin")
        XCTAssertEqual(request.expected, Self.expected)
        XCTAssertEqual(harness.sleeps.count, pendingReads + 1)
        XCTAssertGreaterThan(harness.sleeps.reduce(0, +), 30, "a fetch is not given up after 30 s")
        XCTAssertTrue(harness.sleeps.allSatisfy { $0 == 1 }, "about once a second while the progress is on screen")
        let progress = try XCTUnwrap(sawProgress)
        XCTAssertEqual(progress.title, "Fetching origin…")
        XCTAssertEqual(progress.message, "Counting objects")
        XCTAssertEqual(progress.fraction, 0.01)
        XCTAssertNotNil(progress.elapsed(now: Date()))
        guard case .completed(_, let receipt) = harness.model.mutations.phase else {
            return XCTFail("\(harness.model.mutations.phase)")
        }
        XCTAssertEqual(GitOutcomePresentation.of(harness.model.mutations.phase)?.headline, receipt.outcome?.message)
        XCTAssertNil(harness.model.progress)
        XCTAssertGreaterThanOrEqual(harness.reads, 2, "the status is read after the outcome")
    }

    func testStopWaitingOnlyHidesTheProgressAndKeepsReadingTheReceipt() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(relation: "behind", blocked: "null")
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        harness.replies = [.receipt(Self.receipt("x", state: "pending", operation: "fast_forward"))]
        harness.lookups = (0..<6).map { _ in .receipt(Self.receipt("x", state: "pending", operation: "fast_forward")) }
            + [.receipt(Self.receipt("x", state: "completed", operation: "fast_forward", code: "fast_forwarded", message: "Fast-forwarded main."))]
        var checkedWhileHidden = false
        harness.onSleep = { count in
            guard count == 2 else {
                if count > 2 {
                    XCTAssertNil(harness.model.progress, "the progress view stays dismissed")
                    XCTAssertTrue(harness.model.isRunningInBackground)
                    XCTAssertFalse(harness.model.canMutate, "nothing new starts while Git may still run")
                    XCTAssertFalse(sync.canUpdate)
                    XCTAssertNil(GitOutcomePresentation.of(harness.model.mutations.phase), "no outcome claims Git stopped")
                    checkedWhileHidden = true
                }
                return
            }
            XCTAssertNotNil(harness.model.progress)
            harness.model.stopWaiting()
        }
        await sync.update()
        XCTAssertTrue(checkedWhileHidden)
        XCTAssertEqual(harness.sleeps.prefix(2), [1, 1])
        XCTAssertTrue(harness.sleeps.dropFirst(2).contains { $0 > 1 }, "reads back off once nobody is watching")
        XCTAssertEqual(harness.lookedUp.count, 7, "the receipt is read until it settles")
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
    }

    func testAReceiptLeftPendingIsReadAgainOnReturnAndReconnect() async throws {
        let harness = SyncHarness()
        await harness.model.refresh()
        harness.replies = [.receipt(Self.receipt("x", state: "pending", operation: "fetch"))]
        harness.lookups = [.receipt(Self.receipt("x", state: "pending", operation: "fetch"))]
        harness.onSleep = { count in if count == 2 { harness.online = false } }
        await harness.model.fetch(remote: "origin")
        guard case .pending = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertFalse(harness.model.mutations.isPolling)
        XCTAssertFalse(harness.model.canMutate)

        harness.online = true
        harness.onSleep = nil
        harness.lookups = [.receipt(Self.receipt("x", state: "completed", operation: "fetch", code: "up_to_date", message: "Already up to date."))]
        await harness.model.reconnected()
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertEqual(harness.sent.count, 1, "a pending request is read, never sent again")
        XCTAssertTrue(harness.model.canMutate)
    }

    func testALostMergeIsReadBeforeItIsResentUnderTheSameID() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(relation: "behind", blocked: "null")
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        sync.mode = .merge
        harness.replies = [
            .failure(LatchError.transport("The network connection was lost.")),
            .receipt(Self.receipt("x", state: "completed", operation: "merge", code: "merged", message: "Merged origin/main.")),
        ]
        await sync.update()
        guard case .lost = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertFalse(sync.canUpdate)
        XCTAssertNil(GitOutcomePresentation.of(harness.model.mutations.phase)?.secondaryStep, "no repeat before the receipt")
        let lost = try XCTUnwrap(GitOutcomePresentation.of(
            harness.model.mutations.phase, connectionLostAt: harness.model.mutations.connectionLostAt
        ))
        XCTAssertTrue(lost.timeline.last?.hasPrefix("Connection lost") == true)

        harness.lookups = [.failure(LatchError.git(code: "receipt_not_found", reason: "none"))]
        await harness.model.reconnected()
        XCTAssertEqual(harness.lookedUp, [harness.sent[0].requestId], "the receipt is read first")
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertEqual(harness.sent[0], harness.sent[1], "the same request under the same id")
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
    }

    func testAnUncertainFetchMayBeRepeatedAsANewRequestButAMergeMayNot() async throws {
        let harness = SyncHarness()
        await harness.model.refresh()
        harness.replies = [.receipt(Self.receipt(
            "x", state: "uncertain", operation: "fetch", code: "timed_out", message: "Git ran out of time.",
            progress: Self.progress("Receiving objects", 80)
        ))]
        await harness.model.fetch(remote: "origin")
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(
            harness.model.mutations.phase, lastReceipt: harness.model.mutations.lastReceipt
        ))
        XCTAssertEqual(presentation.style, .uncertain)
        XCTAssertEqual(presentation.secondaryStep, .fetchAgain)
        XCTAssertTrue(presentation.timeline.contains { $0.hasPrefix("Your Mac accepted it") })
        XCTAssertTrue(presentation.timeline.contains { $0.hasPrefix("Git started") })
        XCTAssertTrue(presentation.timeline.contains { $0.hasPrefix("Last progress: Receiving objects") })
        XCTAssertFalse(harness.model.canMutate)

        harness.replies = [.receipt(Self.receipt("y", state: "completed", operation: "fetch", code: "fetched"))]
        await harness.model.fetchAgain()
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertNotEqual(harness.sent[0].requestId, harness.sent[1].requestId, "a new fetch, not a resend")
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }

        let merge = GitMutationAction.merge(GitUpdateRequest(
            requestId: "m", path: "/r", expected: Self.expected, upstream: "origin/main", upstreamOid: Self.upstreamOid
        ))
        let uncertainMerge = try XCTUnwrap(GitOutcomePresentation.of(.uncertain(
            merge, try GitMutationTests.decodeReceipt(Self.receipt("m", state: "uncertain", operation: "merge", code: "interrupted"))
        )))
        XCTAssertNil(uncertainMerge.secondaryStep, "a merge is never offered again from an uncertain outcome")
        XCTAssertEqual(uncertainMerge.nextStep, .refresh)
    }

    // MARK: Outcomes

    func testAStoppedMergeRefreshesTheStatusAndShowsTheOperation() async throws {
        let harness = SyncHarness()
        harness.previewJSON = Self.preview(relation: "diverged")
        await harness.model.refresh()
        let sync = try XCTUnwrap(harness.model.syncModel())
        await sync.load()
        sync.mode = .merge
        harness.replies = [.receipt(Self.receipt(
            "x", state: "completed", operation: "merge", code: "stopped",
            message: "Git stopped on conflicts in 1 file.", files: #"["both.txt"]"#
        ))]
        // What the Mac reports once Git stopped.
        harness.statusJSON = Self.status(
            operation: #"{"kind":"merge","mergeHead":"\#(Self.upstreamOid)","summary":"origin/main"}"#, conflicted: true
        )
        let reads = harness.reads
        await sync.update()
        XCTAssertEqual(harness.reads, reads + 1, "the status is read after the outcome")
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(harness.model.mutations.phase))
        XCTAssertEqual(presentation.style, .stopped)
        XCTAssertEqual(presentation.headline, "Git stopped on conflicts in 1 file")
        XCTAssertEqual(presentation.files, ["both.txt"])
        XCTAssertEqual(presentation.detail, "Resolve on your Mac, then stage them here.")
        let state = try XCTUnwrap(harness.model.operationState)
        XCTAssertEqual(state.controlled, .merge)
        XCTAssertEqual(state.conflictLine, "1 conflicted file")
        XCTAssertEqual(state.refs(branch: harness.model.status?.head.branch), "Merging origin/main into main")
        XCTAssertFalse(harness.model.canContinue)
        XCTAssertTrue(harness.model.canMutate, "a stopped merge is a seen outcome, not a running one")
    }

    func testRefusalsNameTheirNextStep() throws {
        let action = GitMutationAction.fastForward(GitUpdateRequest(
            requestId: "f", path: "/r", expected: Self.expected, upstream: "origin/main", upstreamOid: Self.upstreamOid
        ))
        func outcome(_ code: String) throws -> GitOutcomePresentation {
            let receipt = try GitMutationTests.decodeReceipt(Self.receipt("f", state: "rejected", operation: "fast_forward", code: code, message: "Git refused."))
            return try XCTUnwrap(GitOutcomePresentation.of(.rejected(action, receipt)))
        }
        let notFF = try outcome("not_fast_forward")
        XCTAssertEqual(notFF.nextStep, .chooseMergeOrRebase)
        XCTAssertEqual(notFF.primaryTitle, "Choose Merge or Rebase")
        XCTAssertEqual(notFF.gitOutput, "git said no")
        XCTAssertEqual(try outcome("refresh_required").style, .refreshRequired)
        XCTAssertEqual(try outcome("refresh_required").nextStep, .refresh)
        XCTAssertEqual(try outcome("would_overwrite").nextStep, .commitChanges)
        XCTAssertEqual(try outcome("auth_failed").headline, "The remote didn't accept your Mac's credentials")
        XCTAssertEqual(try outcome("network_failed").headline, "Your Mac couldn't reach the remote")
        let unknown = try outcome("brand_new_code")
        XCTAssertEqual(unknown.detail, "Git refused.", "an unknown code shows the Mac's message")
        XCTAssertEqual(unknown.headline, "Git didn't fast-forward")
    }

    // MARK: Offline, stale, control

    func testSlice3ActionsWaitForTheLinkAFreshStatusAndControl() async throws {
        let harness = SyncHarness(statusJSON: Self.status(operation: #"{"kind":"rebase"}"#))
        await harness.model.refresh()
        XCTAssertTrue(harness.model.canFetch)
        XCTAssertTrue(harness.model.canContinue)

        harness.online = false
        XCTAssertTrue(harness.model.isStale)
        XCTAssertFalse(harness.model.canFetch)
        XCTAssertFalse(harness.model.canContinue)
        XCTAssertFalse(harness.model.canAbort)
        XCTAssertTrue(harness.model.syncBlockReason?.hasPrefix("Reconnecting") == true)
        await harness.model.fetch(remote: "origin")
        await harness.model.continueOperation()
        await harness.model.abortOperation()
        XCTAssertTrue(harness.sent.isEmpty)

        // Back online, but the status on screen is from before: nothing
        // starts until a fresh one is read.
        harness.online = true
        harness.failReads = true
        await harness.model.reconnected()
        XCTAssertFalse(harness.model.canFetch)
        XCTAssertTrue(harness.model.fetchBlockReason?.hasPrefix("Reconnecting") == true)
        harness.failReads = false
        await harness.model.refresh()
        XCTAssertTrue(harness.model.canFetch)

        harness.access = false
        XCTAssertTrue(harness.model.fetchBlockReason?.contains("can't change this repository") == true)
        XCTAssertFalse(harness.model.canAbort)
        await harness.model.abortOperation()
        XCTAssertTrue(harness.sent.isEmpty)
    }

    func testTheHeaderFetchUsesTheUpstreamsRemoteOrAsksAmongSeveral() async throws {
        let harness = SyncHarness()
        await harness.model.refresh()
        let tracked = await harness.model.fetchRemotes()
        XCTAssertEqual(tracked, ["origin"])
        harness.previewJSON = Self.preview(relation: "no_upstream", remote: "null")
        let untracked = await harness.model.fetchRemotes()
        XCTAssertEqual(untracked, ["origin", "fork"], "with no upstream the person picks the remote")
    }

    // MARK: Gateway

    func testGatewayRoutesAndCapabilityChecks() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: GitMutationTests.capabilities(GitMutationTests.allKeys + #","gitSync":true,"gitFetch":true,"gitMerge":true"#))
        StubProtocol.stub(path: "/v2/git/sync", body: Self.preview())
        StubProtocol.stub(method: "POST", path: "/v2/git/fetch", status: 202,
                          body: Self.receipt("x", state: "pending", operation: "fetch", progress: Self.progress("Receiving objects", 10)))
        StubProtocol.stub(method: "POST", path: "/v2/git/merge", body: Self.receipt("x", state: "completed", operation: "merge", code: "merged"))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"), session: StubProtocol.session()
        )
        let preview = try await gateway.gitSync(path: "/r", cursor: "c1")
        XCTAssertEqual(preview.relation, .diverged)
        let query = try XCTUnwrap(StubProtocol.requestQueries.last { $0.0 == "/v2/git/sync" }?.1)
        XCTAssertTrue(query.contains("cursor=c1"), query)

        let fetch = try await gateway.gitMutation(.fetch(GitFetchRequest(requestId: "x", path: "/r", expected: Self.expected, remote: "origin")))
        XCTAssertEqual(fetch.state, .pending, "202 carries the pending receipt")
        XCTAssertEqual(fetch.progress?.percent, 10)
        let update = GitUpdateRequest(requestId: "x", path: "/r", expected: Self.expected, upstream: "origin/main", upstreamOid: Self.upstreamOid)
        let merged = try await gateway.gitMutation(.merge(update))
        XCTAssertEqual(merged.state, .completed)

        do {
            _ = try await gateway.gitMutation(.rebase(update))
            XCTFail("rebase is not advertised")
        } catch LatchError.endpointUnavailable(_) {}
        let conflicted = GitIndexRequest(
            requestId: "x", path: "/r", expected: Self.expected,
            files: [GitFileRef(pathKey: "64", area: .conflicted, fingerprint: "4444444444444444")]
        )
        do {
            _ = try await gateway.gitMutation(.stage(conflicted))
            XCTFail("staging a conflicted file needs gitStageResolved")
        } catch LatchError.endpointUnavailable(_) {}
        XCTAssertFalse(StubProtocol.requests.contains { $0.path == "/v2/git/rebase" || $0.path == "/v2/git/stage" })
        let staleCursor = LatchGateway.error(
            status: 409, path: "/v2/git/sync",
            data: Data(#"{"error":"stale_cursor","reason":"The upstream moved."}"#.utf8)
        )
        XCTAssertEqual(staleCursor, .git(code: "stale_cursor", reason: "The upstream moved."))
    }

    // MARK: Fixtures

    nonisolated static let slice3Keys = #","gitSync":true,"gitFetch":true,"gitFastForward":true,"gitMerge":true,"gitRebase":true,"gitContinue":true,"gitAbort":true,"gitStageResolved":true"#
}

/// A repository model whose Mac is scripted, with the Sync preview.
@MainActor
private final class SyncHarness {
    enum Reply {
        case receipt(String)
        case failure(Error)
    }

    var replies: [Reply] = []
    var lookups: [Reply] = []
    var sent: [GitMutationAction] = []
    var lookedUp: [String] = []
    var reads = 0
    var previewReads = 0
    var syncCursors: [String?] = []
    var sleeps: [Double] = []
    var onSleep: ((Int) -> Void)?
    var online = true
    var access = true
    var failReads = false
    var ids = 0
    var statusJSON: String
    var previewJSON = GitSyncTests.preview()
    var pages: [String: Result<String, Error>] = [:]
    let support: GitMutationSupport
    lazy var model: GitRepositoryModel = makeModel()

    init(support: GitMutationSupport = .everything, statusJSON: String = GitSyncTests.status()) {
        self.support = support
        self.statusJSON = statusJSON
    }

    private func next(_ queue: inout [Reply]) throws -> GitMutationReceipt {
        guard !queue.isEmpty else { throw LatchError.transport("no scripted reply") }
        switch queue.removeFirst() {
        case .receipt(let text): return try GitMutationTests.decodeReceipt(text)
        case .failure(let error): throw error
        }
    }

    private func makeModel() -> GitRepositoryModel {
        GitRepositoryModel(
            folder: "/r",
            read: { [unowned self] _ in
                reads += 1
                if failReads { throw LatchError.transport("offline") }
                return try JSONDecoder().decode(GitStatus.self, from: Data(statusJSON.utf8))
            },
            readDiff: { _, _, _, _ in throw LatchError.transport("unused") },
            hasAccess: { [unowned self] in access },
            isOnline: { [unowned self] in online },
            support: support,
            mutate: { [unowned self] action in
                sent.append(action)
                return try next(&replies)
            },
            lookupReceipt: { [unowned self] id in
                lookedUp.append(id)
                return try next(&lookups)
            },
            readSync: { [unowned self] _, cursor in
                previewReads += 1
                syncCursors.append(cursor)
                let text: String
                if let cursor, let page = pages[cursor] {
                    text = try page.get()
                } else {
                    text = previewJSON
                }
                return try JSONDecoder().decode(GitSyncPreview.self, from: Data(text.utf8))
            },
            makeRequestID: { [unowned self] in
                ids += 1
                return "00000000-0000-0000-0000-\(String(format: "%012d", ids))"
            },
            sleep: { [unowned self] seconds in
                sleeps.append(seconds)
                onSleep?(sleeps.count)
            }
        )
    }
}
