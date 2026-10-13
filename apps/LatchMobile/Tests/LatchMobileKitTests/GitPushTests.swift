import XCTest

@testable import LatchMobileKit

/// Slice 4 of Git management: the push review, the push itself through the
/// repository's one runner, and "Check the remote" for a push the Mac could
/// not confirm. Every response has the gateway's shape.
@MainActor
final class GitPushTests: XCTestCase {
    nonisolated static let head = GitMutationTests.head
    nonisolated static let destinationOid = String(repeating: "d", count: 40)
    nonisolated static let preconditions = GitSyncTests.preconditions
    nonisolated static let expected = GitMutationTests.expected

    nonisolated static func target(
        remote: String = "origin", url: String = #""https://github.com/acme/r.git""#, destination: String = "main",
        oid: String? = destinationOid, selectedBy: String = "upstream", newBranch: Bool = false,
        isDefault: Bool = true
    ) -> String {
        let oidJSON = oid.map { "\"\($0)\"" } ?? "null"
        return """
        {"remote":"\(remote)","remoteUrl":\(url),"destination":"\(destination)",
         "destinationRef":"refs/heads/\(destination)","trackingRef":"refs/remotes/\(remote)/\(destination)",
         "destinationOid":\(oidJSON),"selectedBy":"\(selectedBy)","newBranch":\(newBranch),
         "isDefaultBranch":\(isDefault),"defaultBranch":"main"}
        """
    }

    nonisolated static func preview(
        target: String? = target(), selectionRequired: Bool = false, reason: String = "null",
        suggestion: String = "null", ahead: String = "2", behind: String = "0", blocked: String = "null",
        rejection: String = "null", outgoing: [String] = ["p2", "p1"], cursor: String = "null",
        source: String = #"{"branch":"main","oid":"\#(head)"}"#
    ) -> String {
        let commits = outgoing.map { oid in
            #"{"oid":"\#(oid)","subject":"Commit \#(oid)","author":"Ana","committedAt":"2026-10-13T09:00:00Z"}"#
        }.joined(separator: ",")
        return """
        {"root":"/r","head":{"branch":"main","oid":"\(head)","detached":false},"source":\(source),
         "remotes":[{"name":"origin","url":"https://github.com/acme/r.git"},{"name":"fork","url":null}],
         "target":\(target ?? "null"),"selectionRequired":\(selectionRequired),"selectionReason":\(reason),
         "suggestion":\(suggestion),"lastFetchedAt":"2026-10-13T10:00:00Z","ahead":\(ahead),"behind":\(behind),
         "blockedReason":\(blocked),"rejectionLikelyReason":\(rejection),"outgoing":[\(commits)],
         "nextCursor":\(cursor),"operation":null,"preconditions":\(preconditions)}
        """
    }

    nonisolated static let unselected = preview(
        target: nil, selectionRequired: true,
        reason: #""topic doesn't track a branch on a remote.""#,
        suggestion: #"{"remote":"origin","destination":"topic"}"#, ahead: "null"
    )

    nonisolated static func receipt(_ id: String, state: String, code: String? = nil, message: String = "Pushed.") -> String {
        GitSyncTests.receipt(id, state: state, operation: "push", code: code, message: message)
    }

    nonisolated static func reconciliation(
        _ id: String, state: String, code: String?, checked: Bool = true, message: String = "Checked origin."
    ) -> String {
        """
        {"receipt":\(receipt(id, state: state, code: code)),"remoteChecked":\(checked),
         "remoteOid":\(checked ? "\"\(head)\"" : "null"),"message":"\(message)"}
        """
    }

    nonisolated static let pushKeys = #","gitPushPreview":true,"gitPush":true,"gitPushReconcile":true"#

    private func reviewed(_ harness: PushHarness) async throws -> GitPushModel {
        await harness.model.refresh()
        let push = try XCTUnwrap(harness.model.pushModel())
        await push.load()
        return push
    }

    // MARK: Decoding

    func testPushPreviewDecodesEveryShape() throws {
        let decoder = JSONDecoder()
        let upstream = try decoder.decode(GitPushPreview.self, from: Data(Self.preview(cursor: #""c2""#).utf8))
        let target = try XCTUnwrap(upstream.target)
        XCTAssertEqual(upstream.source, GitPushSource(branch: "main", oid: Self.head))
        XCTAssertEqual(target.selectedBy, .upstream)
        XCTAssertEqual(target.trackingRef, "refs/remotes/origin/main")
        XCTAssertEqual(target.destinationOid, Self.destinationOid)
        XCTAssertTrue(target.isDefaultBranch)
        XCTAssertEqual(upstream.outgoing.map(\.oid), ["p2", "p1"])
        XCTAssertEqual(upstream.nextCursor, "c2")
        XCTAssertEqual(upstream.preconditions, Self.expected)
        XCTAssertEqual(upstream.pushButtonTitle, "Push 2 commits to origin/main")
        XCTAssertEqual(upstream.pushSpokenLabel, "Push 2 commits from main to origin main")
        XCTAssertEqual(upstream.refsLine, "main (this Mac · 0123456) \u{2192} origin/main (remote · ddddddd)")
        XCTAssertEqual(upstream.refsSpoken, "main on this Mac at 0123456, to origin main on the remote at ddddddd")
        XCTAssertNotNil(upstream.defaultBranchNote)
        XCTAssertNil(upstream.newBranchNote)
        XCTAssertEqual(upstream.outgoingHeader, "Outgoing \u{2191}2")

        let choosing = try decoder.decode(GitPushPreview.self, from: Data(Self.unselected.utf8))
        XCTAssertNil(choosing.target)
        XCTAssertTrue(choosing.selectionRequired)
        XCTAssertEqual(choosing.suggestion, GitPushSuggestion(remote: "origin", destination: "topic"))
        XCTAssertNil(choosing.ahead)
        XCTAssertNil(choosing.refsLine)

        let created = try decoder.decode(GitPushPreview.self, from: Data(Self.preview(
            target: Self.target(url: "null", destination: "topic", oid: nil, selectedBy: "request", newBranch: true, isDefault: false),
            ahead: "null"
        ).utf8))
        let createdTarget = try XCTUnwrap(created.target)
        XCTAssertNil(createdTarget.remoteUrl)
        XCTAssertNil(createdTarget.destinationOid)
        XCTAssertEqual(createdTarget.selectedBy, .request)
        XCTAssertEqual(created.pushButtonTitle, "Push main to origin/topic")
        XCTAssertEqual(created.destinationLine, "origin/topic (new branch)")
        XCTAssertNotNil(created.newBranchNote)
        XCTAssertNil(created.defaultBranchNote)
        XCTAssertEqual(created.remoteURL, "https://github.com/acme/r.git", "falls back to the remote's URL")

        let one = try decoder.decode(GitPushPreview.self, from: Data(Self.preview(ahead: "1").utf8))
        XCTAssertEqual(one.pushButtonTitle, "Push 1 commit to origin/main")

        // Optional fields left out entirely, and a selectedBy from the future.
        let sparse = """
        {"root":"/r","head":{"branch":null,"oid":null,"detached":true},"remotes":[],
         "target":{"remote":"origin","destination":"x","destinationRef":"refs/heads/x","selectedBy":"magic",
          "newBranch":false,"isDefaultBranch":false},
         "selectionRequired":false,"outgoing":[],"preconditions":\(Self.preconditions)}
        """
        let minimal = try decoder.decode(GitPushPreview.self, from: Data(sparse.utf8))
        XCTAssertNil(minimal.source)
        XCTAssertFalse(minimal.selectionRequired)
        XCTAssertNil(minimal.blockedReason)
        XCTAssertNil(minimal.rejectionLikelyReason)
        XCTAssertEqual(minimal.target?.selectedBy, .unknown)
        XCTAssertEqual(minimal.target?.newBranch, false)
    }

    func testReconciliationDecodes() throws {
        let settled = try JSONDecoder().decode(
            GitPushReconciliation.self, from: Data(Self.reconciliation("id", state: "completed", code: "pushed").utf8)
        )
        XCTAssertTrue(settled.remoteChecked)
        XCTAssertEqual(settled.remoteOid, Self.head)
        XCTAssertEqual(settled.receipt.state, .completed)
        XCTAssertEqual(settled.receipt.operation, .push)
        let unreachable = try JSONDecoder().decode(
            GitPushReconciliation.self,
            from: Data(Self.reconciliation("id", state: "uncertain", code: "push_unconfirmed", checked: false).utf8)
        )
        XCTAssertFalse(unreachable.remoteChecked)
        XCTAssertNil(unreachable.remoteOid)
    }

    // MARK: Gating

    func testPushIsGatedOnPreviewPushAndReceiptsAndCheckingOnItsOwnKey() throws {
        func support(_ keys: String) throws -> GitMutationSupport {
            GitMutationSupport(endpoints: try JSONDecoder().decode(
                GatewayEndpoints.self,
                from: Data(#"{"sessions":true,"terminal":true,"conversation":true\#(keys)}"#.utf8)
            ))
        }
        XCTAssertFalse(try support(#","gitPushPreview":true,"gitPush":true"#).push, "receipts are required")
        XCTAssertFalse(try support(#","gitReceipts":true,"gitPushPreview":true"#).push)
        XCTAssertFalse(try support(#","gitReceipts":true,"gitPush":true,"gitPushReconcile":true"#).push)
        XCTAssertFalse(try support(#","gitReceipts":true,"gitPush":true,"gitPushReconcile":true"#).pushReconcile)
        let pushOnly = try support(#","gitReceipts":true,"gitPushPreview":true,"gitPush":true"#)
        XCTAssertTrue(pushOnly.push)
        XCTAssertFalse(pushOnly.pushReconcile)
        XCTAssertTrue(pushOnly.any)
        let all = try support(GitMutationTests.allKeys + GitSyncTests.slice3Keys + Self.pushKeys)
        XCTAssertEqual(all, .withPush)
        XCTAssertFalse(try support(GitMutationTests.allKeys + GitSyncTests.slice3Keys).push, "an older Mac")
    }

    func testOlderMacsHideReviewPushAndCheckTheRemote() async throws {
        let older = PushHarness(support: .everything)
        await older.model.refresh()
        XCTAssertTrue(older.model.offersSync)
        XCTAssertFalse(older.model.offersPush)
        XCTAssertNil(older.model.pushModel())

        var noCheck = GitMutationSupport.withPush
        noCheck.pushReconcile = false
        let harness = PushHarness(support: noCheck)
        await harness.model.refresh()
        XCTAssertTrue(harness.model.offersPush)
        XCTAssertFalse(harness.model.mutations.canCheckRemote)
        harness.replies = [.receipt(Self.receipt("x", state: "uncertain", code: "push_unconfirmed", message: "Lost."))]
        let push = try XCTUnwrap(harness.model.pushModel())
        await push.load()
        await push.push()
        XCTAssertFalse(harness.model.offersRemoteCheck)
        let presentation = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertEqual(presentation.nextStep, .refresh, "without the route, no Check the remote")
        XCTAssertNil(presentation.secondaryStep)
        await harness.model.acknowledgeOutcome()
        XCTAssertNotNil(harness.model.mutations.unresolvedPush, "a missing reconciliation capability cannot prove an old push did not run")
        XCTAssertFalse(push.canPush)
    }

    func testOfflineStaleAndNoControlBlockThePush() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        XCTAssertTrue(push.canPush)
        harness.online = false
        XCTAssertTrue(harness.model.isStale)
        XCTAssertEqual(push.pushBlockReason, "Reconnecting to your Mac. Changes wait for the link and a fresh status.")
        await push.push()
        harness.online = true
        harness.access = false
        XCTAssertTrue(push.pushBlockReason?.contains("Control") == true)
        await push.push()
        XCTAssertTrue(harness.sent.isEmpty)
    }

    // MARK: Review

    func testBlockedAndRejectionLikelyFromTheMac() async throws {
        let harness = PushHarness()
        harness.pushJSON = Self.preview(blocked: #""HEAD is detached.""#)
        let push = try await reviewed(harness)
        XCTAssertEqual(push.pushBlockReason, "HEAD is detached.")
        harness.pushJSON = Self.preview(behind: "1", rejection: #""origin/main has 1 commit main doesn't.""#)
        await push.load()
        XCTAssertNil(push.pushBlockReason, "a likely rejection warns; Git decides")
        XCTAssertNotNil(push.preview?.rejectionLikelyReason)
        harness.pushJSON = Self.preview(ahead: "0", outgoing: [])
        await push.load()
        XCTAssertTrue(push.pushBlockReason?.hasPrefix("Nothing to push") == true)
    }

    func testLoadMoreAppendsAndAStaleCursorRestarts() async throws {
        let harness = PushHarness()
        harness.pushJSON = Self.preview(outgoing: ["a", "b"], cursor: #""c2""#)
        let push = try await reviewed(harness)
        XCTAssertTrue(push.hasMore)
        harness.pushPages["c2"] = .success(Self.preview(outgoing: ["c"]))
        await push.loadMore()
        XCTAssertEqual(push.outgoing.map(\.oid), ["a", "b", "c"])
        harness.pushJSON = Self.preview(outgoing: ["x"], cursor: #""c9""#)
        await push.load()
        harness.pushPages["c9"] = .failure(LatchError.git(code: "stale_cursor", reason: "moved"))
        harness.pushJSON = Self.preview(outgoing: ["z", "x"])
        await push.loadMore()
        XCTAssertEqual(push.outgoing.map(\.oid), ["z", "x"])
        XCTAssertEqual(harness.pushReads.suffix(2).map(\.cursor), ["c9", nil])
        XCTAssertNotNil(push.notice)
    }

    func testANoUpstreamBranchNeedsAnExplicitDestination() async throws {
        let harness = PushHarness()
        harness.pushJSON = Self.unselected
        let push = try await reviewed(harness)
        XCTAssertTrue(push.needsSelection)
        XCTAssertEqual(push.draftRemote, "origin", "the suggestion pre-fills")
        XCTAssertEqual(push.draftDestination, "topic")
        XCTAssertNil(push.selection, "pre-filling confirms nothing")
        XCTAssertFalse(push.canPush)
        await push.push()
        XCTAssertTrue(harness.sent.isEmpty, "nothing is pushed to a suggestion")
        XCTAssertNil(harness.pushReads.last?.selection)

        push.draftDestination = "bad name"
        XCTAssertFalse(push.canReviewSelection)
        push.draftRemote = "nowhere"
        XCTAssertEqual(push.selectionBlockReason, "Choose a remote.")
        push.draftRemote = "fork"
        push.draftDestination = "topic"
        // A Mac that answered with a destination nobody confirmed is refused.
        harness.pushJSON = Self.preview(target: Self.target(remote: "origin", destination: "topic", selectedBy: "request"))
        await push.load()
        XCTAssertFalse(push.canPush)

        harness.pushJSON = Self.preview(
            target: Self.target(remote: "fork", url: "null", destination: "topic", oid: nil, selectedBy: "request", newBranch: true, isDefault: false),
            ahead: "null"
        )
        await push.reviewSelection()
        XCTAssertEqual(harness.pushReads.last?.selection, GitPushSuggestion(remote: "fork", destination: "topic"))
        XCTAssertTrue(push.canPush)
        XCTAssertEqual(push.preview?.pushButtonTitle, "Push main to fork/topic")
        harness.replies = [.receipt(Self.receipt("x", state: "completed", code: "pushed"))]
        await push.push()
        guard case .push(let request)? = harness.sent.last else { return XCTFail("\(harness.sent)") }
        XCTAssertEqual(request.remote, "fork")
        XCTAssertEqual(request.destination, "topic")
        XCTAssertNil(request.remoteUrl)
        XCTAssertNil(request.destinationOid)
        XCTAssertEqual(harness.pushReads.last?.selection, GitPushSuggestion(remote: "fork", destination: "topic"),
                       "the review after the push reads the same destination")
    }

    // MARK: Request

    func testTheRequestIsBuiltFromTheReviewedPreviewOnly() async throws {
        let harness = PushHarness()
        harness.pushJSON = Self.preview(target: Self.target(url: "null", oid: nil))
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "completed", code: "pushed"))]
        await push.push()
        guard case .push(let request)? = harness.sent.first else { return XCTFail("\(harness.sent)") }
        XCTAssertEqual(request, GitPushRequest(
            requestId: "00000000-0000-0000-0000-000000000001", path: "/r", expected: Self.expected,
            remote: "origin", remoteUrl: nil, destination: "main", sourceBranch: "main", sourceOid: Self.head,
            destinationOid: nil
        ))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertTrue(json["remoteUrl"] is NSNull, "absent values are sent as null, not left out")
        XCTAssertTrue(json["destinationOid"] is NSNull)
        XCTAssertEqual(Set(json.keys), [
            "requestId", "path", "expected", "remote", "remoteUrl", "destination", "sourceBranch", "sourceOid",
            "destinationOid",
        ])
    }

    func testTheGatewayRoutes() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: GitMutationTests.capabilities(
            GitMutationTests.allKeys + GitSyncTests.slice3Keys + Self.pushKeys
        ))
        StubProtocol.stub(path: "/v2/git/push/preview", body: Self.preview())
        StubProtocol.stub(method: "POST", path: "/v2/git/push", status: 202, body: Self.receipt("r1", state: "pending"))
        StubProtocol.stub(
            method: "POST", path: "/v2/git/push/reconcile",
            body: Self.reconciliation("r1", state: "completed", code: "pushed")
        )
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"), session: StubProtocol.session()
        )
        let preview = try await gateway.gitPushPreview(path: "/r", remote: "fork", destination: "a/b", cursor: "c2")
        XCTAssertEqual(preview.target?.remote, "origin")
        let query = try XCTUnwrap(StubProtocol.requestQueries.last { $0.0 == "/v2/git/push/preview" }?.1)
        XCTAssertTrue(query.contains("remote=fork"))
        XCTAssertTrue(query.contains("destination=a/b") || query.contains("destination=a%2Fb"))
        XCTAssertTrue(query.contains("cursor=c2"))
        _ = try await gateway.gitPushPreview(path: "/r", remote: "fork", destination: nil)
        let alone = try XCTUnwrap(StubProtocol.requestQueries.last?.1)
        XCTAssertFalse(alone.contains("remote="), "remote and destination go together or not at all")

        let request = GitPushRequest(
            requestId: "r1", path: "/r", expected: Self.expected, remote: "origin", remoteUrl: nil,
            destination: "main", sourceBranch: "main", sourceOid: Self.head, destinationOid: nil
        )
        let pending = try await gateway.gitPush(request)
        XCTAssertEqual(pending.state, .pending)
        let post = try XCTUnwrap(StubProtocol.requests.last { $0.path == "/v2/git/push" })
        XCTAssertTrue(post.body.contains(#""remoteUrl":null"#))
        XCTAssertTrue(post.body.contains(#""destinationOid":null"#))

        let checked = try await gateway.gitPushReconcile(requestId: "r1")
        XCTAssertEqual(checked.receipt.outcome?.code, "pushed")
        let reconcile = try XCTUnwrap(StubProtocol.requests.last { $0.path == "/v2/git/push/reconcile" })
        XCTAssertEqual(reconcile.body, #"{"requestId":"r1"}"#)

        StubProtocol.stub(
            method: "POST", path: "/v2/git/push/reconcile", status: 409,
            body: #"{"error":"still_running","reason":"Git is still pushing."}"#
        )
        do {
            _ = try await gateway.gitPushReconcile(requestId: "r1")
            XCTFail("expected still_running")
        } catch {
            XCTAssertEqual(error as? LatchError, .git(code: "still_running", reason: "Git is still pushing."))
        }
    }

    func testAnOlderMacIsNeverSentAPush() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: GitMutationTests.capabilities(GitMutationTests.allKeys))
        let gateway = LatchGateway(
            link: try GatewayLink(address: "https://mac.local:8787", token: "token"), session: StubProtocol.session()
        )
        do {
            _ = try await gateway.gitPushPreview(path: "/r")
            XCTFail("expected endpointUnavailable")
        } catch {
            XCTAssertEqual(error as? LatchError, .endpointUnavailable(.gitPushPreview))
        }
        XCTAssertFalse(StubProtocol.requests.contains { $0.path.hasPrefix("/v2/git/push") })
    }

    // MARK: Runner

    func testPendingIsPolledToCompletedThenEverythingIsReadAgain() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        let readsBefore = harness.reads
        let previewsBefore = harness.pushReads.count
        harness.replies = [.receipt(Self.receipt("x", state: "pending"))]
        harness.lookups = [.receipt(Self.receipt("x", state: "pending")), .receipt(Self.receipt("x", state: "completed", code: "pushed", message: "Pushed 2 commits to origin/main."))]
        await push.push()
        guard case .completed(.push, let receipt) = harness.model.mutations.phase else {
            return XCTFail("\(harness.model.mutations.phase)")
        }
        XCTAssertEqual(receipt.outcome?.code, "pushed")
        XCTAssertEqual(harness.lookedUp.count, 2)
        XCTAssertEqual(harness.reads, readsBefore + 1, "the status is read again")
        XCTAssertEqual(harness.pushReads.count, previewsBefore + 1, "and the review")
        XCTAssertEqual(harness.model.outcomePresentation?.headline, "Pushed 2 commits to origin/main.")
        XCTAssertEqual(
            GitOutcomePresentation.completedFallback(harness.sent[0], try GitMutationTests.decodeReceipt(Self.receipt("x", state: "completed", code: "up_to_date"))),
            "origin/main is already up to date."
        )
    }

    func testRefreshRequiredReviewsAgainAndOnlyAConfirmedPushIsANewRequest() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "rejected", code: "refresh_required", message: "main moved."))]
        await push.push()
        let presentation = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertEqual(presentation.style, .refreshRequired)
        XCTAssertEqual(presentation.headline, "Repository changed on your Mac")
        XCTAssertEqual(presentation.nextStep, .reviewPushAgain)
        let reads = harness.pushReads.count
        await push.reviewAgain()
        XCTAssertEqual(harness.pushReads.count, reads + 1)
        XCTAssertEqual(harness.sent.count, 1, "reviewing again sends nothing")
        guard case .idle = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        harness.replies = [.receipt(Self.receipt("y", state: "completed", code: "pushed"))]
        await push.push()
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertNotEqual(harness.sent[0].requestId, harness.sent[1].requestId)
    }

    func testALostAnswerReadsTheReceiptFirstAndResendsTheSameRequest() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.failure(LatchError.transport("gone"))]
        await push.push()
        guard case .lost(.push) = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertFalse(push.canPush)
        XCTAssertFalse(harness.model.offersRemoteCheck, "nothing is known yet; the receipt comes first")
        harness.lookups = [.failure(LatchError.git(code: "receipt_not_found", reason: "never seen"))]
        harness.replies = [.receipt(Self.receipt("x", state: "completed", code: "pushed"))]
        await harness.model.reconnected()
        XCTAssertEqual(harness.lookedUp, [harness.sent[0].requestId])
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertEqual(harness.sent[0], harness.sent[1], "the same request, same id")
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
    }

    func testUncertainResolvesToPushedOnTheMacsEvidence() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "uncertain", code: "push_unconfirmed", message: "The connection to origin dropped."))]
        await push.push()
        let uncertain = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertEqual(uncertain.style, .uncertain)
        XCTAssertEqual(uncertain.primaryTitle, "Check the remote")
        XCTAssertEqual(uncertain.nextStep, .checkRemote)
        XCTAssertNotEqual(uncertain.secondaryStep, .pushAgain)
        XCTAssertTrue(uncertain.timeline.first?.contains("Receipt") == true)
        XCTAssertTrue(harness.model.offersRemoteCheck)
        XCTAssertFalse(push.offersPushAgain)
        XCTAssertFalse(push.canPush)

        let reads = harness.reads
        harness.checks = [.success(Self.reconciliation(harness.sent[0].requestId, state: "completed", code: "pushed"))]
        harness.onCheck = {
            XCTAssertEqual(harness.model.outcomePresentation?.isChecking, true, "the button is off while checking")
            XCTAssertEqual(harness.model.outcomePresentation?.headline, "Checking the remote…")
        }
        await push.checkRemote()
        XCTAssertEqual(harness.checked, [harness.sent[0].requestId])
        guard case .completed(.push, _) = harness.model.mutations.phase else {
            return XCTFail("\(harness.model.mutations.phase)")
        }
        XCTAssertGreaterThan(harness.reads, reads)
        XCTAssertEqual(harness.sent.count, 1)
    }

    func testNotAppliedOffersPushAgainAsANewRequest() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "uncertain", code: "push_unconfirmed"))]
        await push.push()
        harness.checks = [.success(Self.reconciliation(harness.sent[0].requestId, state: "rejected", code: "not_applied"))]
        await harness.model.checkRemote()
        let presentation = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertEqual(presentation.headline, "No sign the push ran")
        XCTAssertEqual(presentation.primaryTitle, "Push again")
        XCTAssertEqual(presentation.nextStep, .pushAgain)
        XCTAssertTrue(push.offersPushAgain)
        harness.replies = [.receipt(Self.receipt("y", state: "completed", code: "pushed"))]
        await push.pushAgain()
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertNotEqual(harness.sent[0].requestId, harness.sent[1].requestId)
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
    }

    func testPushAgainStopsWhenTheReviewChanged() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "rejected", code: "not_applied"))]
        await push.push()
        harness.pushJSON = Self.preview(target: Self.target(oid: String(repeating: "f", count: 40)))
        await push.pushAgain()
        XCTAssertEqual(harness.sent.count, 1, "a different review goes back to the person")
        XCTAssertNotNil(push.notice)
    }

    func testOnlyNotAppliedOffersPushAgain() async throws {
        for code in ["non_fast_forward", "remote_rejected", "auth_failed", "network_failed", "hook_failed", "git_refused", "remote_not_found"] {
            let harness = PushHarness()
            let push = try await reviewed(harness)
            harness.replies = [.receipt(Self.receipt("x", state: "rejected", code: code, message: "No."))]
            await push.push()
            let presentation = try XCTUnwrap(harness.model.outcomePresentation, code)
            XCTAssertEqual(presentation.style, .rejected, code)
            XCTAssertNotEqual(presentation.nextStep, .pushAgain, code)
            XCTAssertFalse(push.offersPushAgain, code)
            XCTAssertEqual(presentation.gitOutput, "git said no", code)
            await push.pushAgain()
            XCTAssertEqual(harness.sent.count, 1, code)
        }
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "rejected", code: "non_fast_forward", message: "Fetch first."))]
        await push.push()
        let presentation = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertEqual(presentation.primaryTitle, "Fetch and choose how to update")
        XCTAssertEqual(presentation.nextStep, .fetchAndChooseUpdate)
    }

    func testStillUncertainAfterTheCheckOffersNoPushAgain() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "uncertain", code: "push_unconfirmed"))]
        await push.push()
        let id = harness.sent[0].requestId
        harness.checks = [.success(Self.reconciliation(id, state: "uncertain", code: "push_unconfirmed", checked: false, message: "Couldn't reach origin."))]
        await harness.model.checkRemote()
        guard case .uncertain = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        let presentation = try XCTUnwrap(harness.model.outcomePresentation)
        XCTAssertTrue(presentation.detail.contains("Couldn't reach origin."))
        XCTAssertEqual(presentation.nextStep, .checkRemote)
        XCTAssertNotEqual(presentation.secondaryStep, .pushAgain)
        XCTAssertFalse(push.offersPushAgain)

        // The check itself failing infers nothing either.
        harness.checks = [.failure(LatchError.transport("gone"))]
        await harness.model.checkRemote()
        guard case .uncertain = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertTrue(harness.model.outcomePresentation?.detail.contains("couldn't check the remote") == true)
        harness.checks = [.failure(LatchError.git(code: "receipt_not_found", reason: "gone"))]
        await harness.model.checkRemote()
        guard case .uncertain = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertEqual(harness.sent.count, 1, "nothing is ever resent from an uncertain push")
    }

    func testAnUncertainPushPutAsideBlocksOnlyTheNextPush() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "uncertain", code: "push_unconfirmed"))]
        await push.push()
        XCTAssertFalse(harness.model.canMutate)
        await harness.model.acknowledgeOutcome()
        guard case .idle = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertNotNil(harness.model.mutations.unresolvedPush)
        XCTAssertTrue(harness.model.canMutate, "other changes may go on")
        XCTAssertEqual(push.pushBlockReason, "Check the remote for your last push before pushing again.")
        XCTAssertTrue(harness.model.offersRemoteCheck)
        harness.checks = [.success(Self.reconciliation(harness.sent[0].requestId, state: "completed", code: "already_on_remote"))]
        await push.checkRemote()
        XCTAssertNil(harness.model.mutations.unresolvedPush)
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        harness.model.mutations.acknowledge()
        harness.pushJSON = Self.preview()
        await push.load()
        XCTAssertTrue(push.canPush)
    }

    func testStillRunningKeepsWaitingAndStopWaitingStopsNothing() async throws {
        let harness = PushHarness()
        let push = try await reviewed(harness)
        harness.replies = [.receipt(Self.receipt("x", state: "pending"))]
        // The link drops while waiting: the receipt stays pending.
        harness.onSleep = { _ in harness.online = false }
        await push.push()
        harness.onSleep = nil
        harness.online = true
        guard case .pending(.push, _) = harness.model.mutations.phase else {
            return XCTFail("\(harness.model.mutations.phase)")
        }
        let progress = try XCTUnwrap(harness.model.progress)
        XCTAssertEqual(progress.title, "Pushing main to origin/main…")
        XCTAssertTrue(progress.stopWaitingNote.contains("doesn't stop the push"))
        harness.model.stopWaiting()
        XCTAssertNil(harness.model.progress)
        XCTAssertTrue(harness.model.isRunningInBackground)
        guard case .pending = harness.model.mutations.phase else {
            return XCTFail("Stop waiting changed the phase: \(harness.model.mutations.phase)")
        }
        XCTAssertNil(harness.model.outcomePresentation, "nothing says it stopped")
        XCTAssertFalse(push.canPush)
        XCTAssertTrue(harness.model.offersRemoteCheck, "a push no longer being read can be checked")

        let id = harness.sent[0].requestId
        harness.checks = [.failure(LatchError.git(code: "still_running", reason: "Git is still pushing."))]
        harness.lookups = [.receipt(Self.receipt(id, state: "pending")), .receipt(Self.receipt(id, state: "completed", code: "pushed"))]
        await harness.model.checkRemote()
        XCTAssertEqual(harness.checked, [id])
        XCTAssertEqual(harness.lookedUp, [id, id], "Latch kept reading the receipt")
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertEqual(harness.sent.count, 1)
    }
}

/// A repository model whose Mac is scripted, push routes included.
@MainActor
private final class PushHarness {
    enum Reply {
        case receipt(String)
        case failure(Error)
    }

    var replies: [Reply] = []
    var lookups: [Reply] = []
    var checks: [Result<String, Error>] = []
    var onCheck: (() -> Void)?
    var sent: [GitMutationAction] = []
    var lookedUp: [String] = []
    var checked: [String] = []
    var reads = 0
    var pushReads: [(selection: GitPushSuggestion?, cursor: String?)] = []
    var onSleep: ((Int) -> Void)?
    var sleeps = 0
    var online = true
    var access = true
    var ids = 0
    var pushJSON = GitPushTests.preview()
    var pushPages: [String: Result<String, Error>] = [:]
    let support: GitMutationSupport
    lazy var model: GitRepositoryModel = makeModel()

    init(support: GitMutationSupport = .withPush) {
        self.support = support
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
                return try JSONDecoder().decode(GitStatus.self, from: Data(GitSyncTests.status().utf8))
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
            readSync: { _, _ in
                try JSONDecoder().decode(GitSyncPreview.self, from: Data(GitSyncTests.preview().utf8))
            },
            readPush: { [unowned self] _, selection, cursor in
                pushReads.append((selection, cursor))
                let text: String
                if let cursor, let page = pushPages[cursor] {
                    text = try page.get()
                } else {
                    text = pushJSON
                }
                return try JSONDecoder().decode(GitPushPreview.self, from: Data(text.utf8))
            },
            checkRemote: { [unowned self] id in
                checked.append(id)
                onCheck?()
                onCheck = nil
                guard !checks.isEmpty else { throw LatchError.transport("no scripted check") }
                let text = try checks.removeFirst().get()
                return try JSONDecoder().decode(GitPushReconciliation.self, from: Data(text.utf8))
            },
            makeRequestID: { [unowned self] in
                ids += 1
                return "00000000-0000-0000-0000-\(String(format: "%012d", ids))"
            },
            sleep: { [unowned self] _ in
                sleeps += 1
                onSleep?(sleeps)
            }
        )
    }
}
