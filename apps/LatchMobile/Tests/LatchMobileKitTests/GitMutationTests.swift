import XCTest

@testable import LatchMobileKit

/// Slice 2 of Git management: staging, commit and branches, each through a
/// durable receipt on the Mac. Every response has the gateway's shape.
@MainActor
final class GitMutationTests: XCTestCase {
    nonisolated static let head = "0123456789abcdef0123456789abcdef01234567"
    nonisolated static let index = String(repeating: "a", count: 64)
    nonisolated static let worktree = String(repeating: "b", count: 64)
    nonisolated static let expected = GitPreconditions(branch: "main", head: head, index: index, worktree: worktree)

    nonisolated static func status(staged: Bool = true, operation: String = "null", sessionsActive: Bool = true) -> String {
        let stagedEntry = staged ? """
          {"path":"App/Picker.swift","pathKey":"61","origPath":null,"origPathKey":null,"area":"staged",
           "change":"modified","conflict":null,"submodule":false,"additions":3,"deletions":1,"binary":false,
           "fingerprint":"1111111111111111"},
        """ : ""
        return """
        {"root":"/r","name":"r","kind":"repository",
         "head":{"branch":"main","oid":"\(head)","detached":false},
         "upstream":null,"lastFetchedAt":null,"operation":\(operation),
         "entries":[\(stagedEntry)
          {"path":"README.md","pathKey":"62","origPath":null,"origPathKey":null,"area":"unstaged",
           "change":"modified","conflict":null,"submodule":false,"additions":1,"deletions":0,"binary":false,
           "fingerprint":"2222222222222222"},
          {"path":"new file.txt","pathKey":"63","origPath":null,"origPathKey":null,"area":"untracked",
           "change":"untracked","conflict":null,"submodule":false,"additions":null,"deletions":null,"binary":false,
           "fingerprint":"3333333333333333"},
          {"path":"both.txt","pathKey":"64","origPath":null,"origPathKey":null,"area":"conflicted",
           "change":"conflicted","conflict":"UU","submodule":false,"additions":null,"deletions":null,"binary":false,
           "fingerprint":"4444444444444444"}
         ],
         "entriesTruncated":false,"totalEntries":4,
         "sessions":[{"id":"ses_1","name":"git-ui","title":"Agent","commandLabel":"claude","connector":"claude",
           "agentPhase":"working","idleMs":0,"active":\(sessionsActive)}],
         "observedAt":"2026-10-13T10:04:00Z",
         "preconditions":{"branch":"main","head":"\(head)","index":"\(index)","worktree":"\(worktree)"}}
        """
    }

    nonisolated static func receipt(
        _ id: String, state: String, operation: String = "stage", code: String = "staged",
        message: String = "Staged README.md.", output: String = "null", files: String = "[]"
    ) -> String {
        """
        {"requestId":"\(id)","operation":"\(operation)","root":"/r","state":"\(state)",
         "acceptedAt":"2026-10-13T10:05:00Z","finishedAt":null,
         "outcome":{"code":"\(code)","message":"\(message)","gitOutput":\(output),"gitOutputTruncated":false,
          "head":null,"branch":"main","commit":null,"files":\(files),"reconciled":false}}
        """
    }

    nonisolated static func decodeReceipt(_ text: String) throws -> GitMutationReceipt {
        try JSONDecoder().decode(GitMutationReceipt.self, from: Data(text.utf8))
    }

    // MARK: Decoding and encoding

    func testStatusWithPreconditionsAndAnOlderStatusWithout() throws {
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(Self.status().utf8))
        XCTAssertEqual(status.preconditions, Self.expected)
        XCTAssertEqual(status.entries[0].fingerprint, "1111111111111111")
        // A slice-1 Mac sends neither; the status still decodes, without them.
        let older = try JSONDecoder().decode(GitStatus.self, from: Data(GitManagementTests.status.utf8))
        XCTAssertNil(older.preconditions)
        XCTAssertNil(older.entries[0].fingerprint)
    }

    func testReceiptToleratesUnknownStatesAndOperations() throws {
        let receipt = try Self.decodeReceipt(Self.receipt("id", state: "completed"))
        XCTAssertEqual(receipt.state, .completed)
        XCTAssertEqual(receipt.operation, .stage)
        XCTAssertEqual(receipt.outcome?.code, "staged")
        let future = try Self.decodeReceipt(Self.receipt("id", state: "half_done", operation: "teleport", code: "brand_new"))
        XCTAssertEqual(future.state, .uncertain, "an unknown state must be read before acting again")
        XCTAssertEqual(future.operation, .unknown)
        let pending = try Self.decodeReceipt(#"{"requestId":"x","operation":"commit","root":"/r","state":"pending","acceptedAt":"t","finishedAt":null,"outcome":null}"#)
        XCTAssertEqual(pending.state, .pending)
        XCTAssertNil(pending.outcome)
    }

    func testBranchListDecodes() throws {
        let list = try JSONDecoder().decode(GitBranchList.self, from: Data(Self.branches.utf8))
        XCTAssertEqual(list.branches.map(\.name), ["main", "feature", "elsewhere"])
        XCTAssertEqual(list.branches[1].upstreamLine, "origin/feature · \u{2191}2 \u{2193}1")
        XCTAssertEqual(list.branches[2].upstreamLine, "origin/old is gone")
        XCTAssertTrue(list.branches[2].checkedOutElsewhere)
    }

    func testPreconditionsEncodeNullBranchAndHeadExplicitly() throws {
        let request = GitIndexRequest(
            requestId: "id", path: "/r",
            expected: GitPreconditions(branch: nil, head: nil, index: Self.index, worktree: Self.worktree),
            all: [.unstaged]
        )
        let json = try XCTUnwrap(String(data: JSONEncoder().encode(request), encoding: .utf8))
        XCTAssertTrue(json.contains(#""branch":null"#), json)
        XCTAssertTrue(json.contains(#""head":null"#), json)
        XCTAssertTrue(json.contains(#""all":["unstaged"]"#), json)
        let create = try JSONEncoder().encode(GitCreateBranchRequest(
            requestId: "id", path: "/r", expected: Self.expected, name: "x", startPoint: "main",
            startOid: Self.head, switch: true
        ))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: create) as? [String: Any])
        XCTAssertEqual(object["switch"] as? Bool, true)
        XCTAssertEqual(object["startOid"] as? String, Self.head)
    }

    // MARK: Capability gating

    func testEachMutationIsGatedOnItsOwnKeyAndOnReceipts() throws {
        func endpoints(_ keys: String) throws -> GatewayEndpoints {
            try JSONDecoder().decode(
                GatewayEndpoints.self,
                from: Data(#"{"sessions":true,"terminal":true,"conversation":true\#(keys)}"#.utf8)
            )
        }
        XCTAssertEqual(GitMutationSupport(endpoints: try endpoints("")), .none)
        XCTAssertEqual(
            GitMutationSupport(endpoints: try endpoints(#","gitStage":true,"gitCommit":true"#)), .none,
            "without receipts a lost answer could not be checked"
        )
        let partial = GitMutationSupport(endpoints: try endpoints(#","gitStage":true,"gitReceipts":true,"gitSwitchBranch":true"#))
        XCTAssertTrue(partial.stage)
        XCTAssertFalse(partial.unstage)
        XCTAssertFalse(partial.commit)
        XCTAssertFalse(partial.switchBranch, "switching needs the branch list too")
        XCTAssertEqual(GitMutationSupport(endpoints: try endpoints(Self.allKeys)), .all)
    }

    func testAnOlderStatusOffersNoChangesEvenOnANewerMac() async throws {
        let older = try JSONDecoder().decode(GitStatus.self, from: Data(GitManagementTests.status.utf8))
        let harness = Harness(status: older)
        await harness.model.refresh()
        XCTAssertFalse(harness.model.offersChanges)
        XCTAssertFalse(harness.model.offersStaging(older.entries[2]))
        XCTAssertNil(harness.model.mutationBlockReason)
    }

    func testThePairedModelWiresSupportFromDiscovery() async throws {
        StubProtocol.reset()
        StubProtocol.stub(path: "/v2/capabilities", body: Self.capabilities(Self.allKeys))
        StubProtocol.stub(path: "/v2/sessions", body: #"{"sessions":[]}"#)
        StubProtocol.stub(path: "/v2/git/status", body: Self.status())
        StubProtocol.stub(method: "POST", path: "/v2/git/stage", body: Self.receipt("x", state: "completed"))
        let model = makePairedModel()
        await model.connectPairedDevice(pairedRecord())
        XCTAssertEqual(model.gitMutationSupport, .all)
        let opened = await model.gitRepository(folder: "/r")
        let repository = try XCTUnwrap(opened)
        XCTAssertTrue(repository.offersChanges)
        let readme = try XCTUnwrap(repository.status?.entries.first { $0.path == "README.md" })
        await repository.toggleStaging(readme)
        let post = try XCTUnwrap(StubProtocol.requests.last { $0.method == "POST" })
        XCTAssertEqual(post.path, "/v2/git/stage")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(post.body.utf8)) as? [String: Any])
        let files = try XCTUnwrap(body["files"] as? [[String: Any]])
        XCTAssertEqual(files.first?["pathKey"] as? String, "62")
        XCTAssertEqual(files.first?["fingerprint"] as? String, "2222222222222222")
        XCTAssertEqual((body["expected"] as? [String: Any])?["index"] as? String, Self.index)
        guard case .completed = repository.mutations.phase else {
            return XCTFail("expected completed, got \(repository.mutations.phase)")
        }
    }

    func testGatewayMapsReceiptNotFoundAndConflictsToTypedErrors() {
        let missing = LatchGateway.error(
            status: 404, path: "/v2/git/receipts/abc",
            data: Data(#"{"error":"receipt_not_found","reason":"No such request."}"#.utf8)
        )
        XCTAssertEqual(missing, .git(code: "receipt_not_found", reason: "No such request."))
        let conflict = LatchGateway.error(
            status: 409, path: "/v2/git/commit",
            data: Data(#"{"error":"request_id_conflict","reason":"different"}"#.utf8)
        )
        XCTAssertEqual(conflict, .git(code: "request_id_conflict", reason: "different"))
    }

    // MARK: Runner transitions

    func testSuccessRefreshesTheStatus() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.receipt(Self.receipt("id-1", state: "completed"))]
        let readme = try XCTUnwrap(harness.model.status?.entries[1])
        await harness.model.toggleStaging(readme)
        guard case .completed(let action, _) = harness.model.mutations.phase else {
            return XCTFail("\(harness.model.mutations.phase)")
        }
        XCTAssertEqual(action.kind, .stage)
        XCTAssertEqual(harness.reads, 2, "the status is read again after the outcome")
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(harness.model.mutations.phase))
        XCTAssertEqual(presentation.style, .completed)
        XCTAssertEqual(presentation.headline, "Staged README.md.")
        XCTAssertTrue(harness.model.canMutate, "a completed outcome does not block the next change")
    }

    func testRefreshRequiredIsItsOwnPresentation() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.receipt(Self.receipt("id-1", state: "rejected", operation: "commit", code: "refresh_required", message: "The index changed."))]
        await harness.model.commit(summary: "Fix\nthe thing", description: "  ")
        let sent = try XCTUnwrap(harness.sent.first)
        guard case .commit(let request) = sent else { return XCTFail("\(sent)") }
        XCTAssertEqual(request.summary, "Fix the thing", "the summary is one line")
        XCTAssertNil(request.description)
        XCTAssertEqual(request.expected, Self.expected)
        XCTAssertTrue(harness.model.mutations.phase.isRefreshRequired)
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(harness.model.mutations.phase))
        XCTAssertEqual(presentation.style, .refreshRequired)
        XCTAssertEqual(presentation.headline, "Repository changed on your Mac")
        XCTAssertEqual(presentation.nextStep, .refresh)
    }

    func testAWouldOverwriteRefusalCarriesGitsFiles() async throws {
        let harness = Harness()
        await harness.model.refresh()
        let branches = try XCTUnwrap(harness.model.branchesModel())
        await branches.load()
        XCTAssertEqual(branches.current?.name, "main")
        XCTAssertEqual(branches.others.map(\.name), ["feature", "elsewhere"])
        branches.query = "feat"
        XCTAssertEqual(branches.others.map(\.name), ["feature"])
        let elsewhere = try XCTUnwrap(branches.list?.branches[2])
        XCTAssertFalse(branches.canSwitch(to: elsewhere), "Git refuses a branch checked out in another worktree")

        harness.replies = [.receipt(Self.receipt(
            "id-1", state: "rejected", operation: "switch_branch", code: "would_overwrite",
            message: "Git didn't switch because local changes would be overwritten.",
            output: #""error: Your local changes...\n\tREADME.md""#, files: #"["README.md"]"#
        ))]
        let feature = try XCTUnwrap(branches.others.first)
        await branches.switchBranch(to: feature)
        guard case .switchBranch(let request) = try XCTUnwrap(harness.sent.first) else { return XCTFail() }
        XCTAssertEqual(request.name, "feature")
        XCTAssertEqual(request.oid, String(repeating: "c", count: 40))
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(harness.model.mutations.phase))
        XCTAssertEqual(presentation.style, .rejected)
        XCTAssertEqual(presentation.files, ["README.md"])
        XCTAssertEqual(presentation.nextStep, .commitChanges)
        XCTAssertNotNil(presentation.gitOutput)
        XCTAssertEqual(harness.branchReads, 2, "the branch list is read again after the outcome")
    }

    func testCreateBranchStartsAtTheReviewedCommit() async throws {
        let harness = Harness()
        await harness.model.refresh()
        let branches = try XCTUnwrap(harness.model.branchesModel())
        await branches.load()
        await branches.createBranch(named: "-bad", switchToIt: true)
        XCTAssertTrue(harness.sent.isEmpty, "an implausible name is not sent")
        harness.replies = [.receipt(Self.receipt("id-1", state: "completed", operation: "create_branch", code: "branch_created"))]
        await branches.createBranch(named: " topic/x ", switchToIt: false)
        guard case .createBranch(let request) = try XCTUnwrap(harness.sent.first) else { return XCTFail() }
        XCTAssertEqual(request.name, "topic/x")
        XCTAssertEqual(request.startPoint, "main")
        XCTAssertEqual(request.startOid, Self.head)
        XCTAssertFalse(request.switch)
    }

    func testALostAnswerIsReconciledFromTheReceiptBeforeAnythingElse() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.failure(LatchError.transport("The network connection was lost."))]
        let readme = try XCTUnwrap(harness.model.status?.entries[1])
        await harness.model.toggleStaging(readme)
        guard case .lost = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertFalse(harness.model.canMutate)
        XCTAssertNotNil(harness.model.mutationBlockReason)
        // Another action now is ignored: nothing new is sent.
        await harness.model.applyToSection(.changes)
        XCTAssertEqual(harness.sent.count, 1)
        XCTAssertEqual(GitOutcomePresentation.of(harness.model.mutations.phase)?.nextStep, .checkReceipt)

        harness.lookups = [.receipt(Self.receipt(harness.sent[0].requestId, state: "completed"))]
        await harness.model.reconnected()
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        XCTAssertEqual(harness.lookedUp, [harness.sent[0].requestId])
        XCTAssertEqual(harness.sent.count, 1, "a found receipt is never sent again")
    }

    func testAReceiptTheMacNeverRecordedIsResentUnderTheSameID() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.failure(LatchError.transport("lost")), .receipt(Self.receipt("id", state: "completed"))]
        await harness.model.applyToSection(.untracked)
        harness.lookups = [.failure(LatchError.git(code: "receipt_not_found", reason: "none"))]
        await harness.model.mutations.reconcile()
        XCTAssertEqual(harness.sent.count, 2)
        XCTAssertEqual(harness.sent[0], harness.sent[1], "the same request, the same id")
        guard case .stage(let request) = harness.sent[1] else { return XCTFail() }
        XCTAssertEqual(request.all, [.untracked])
        guard case .completed = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
    }

    func testAPendingReceiptIsPolledUntilItSettles() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.receipt(Self.receipt("id", state: "pending"))]
        harness.lookups = [
            .receipt(Self.receipt("id", state: "pending")),
            .receipt(Self.receipt("id", state: "uncertain", code: "interrupted", message: "The gateway stopped while Git ran.")),
        ]
        await harness.model.applyToSection(.staged)
        XCTAssertEqual(harness.sleeps, 2)
        guard case .uncertain = harness.model.mutations.phase else { return XCTFail("\(harness.model.mutations.phase)") }
        let presentation = try XCTUnwrap(GitOutcomePresentation.of(harness.model.mutations.phase))
        XCTAssertEqual(presentation.style, .uncertain)
        XCTAssertEqual(presentation.detail, "The gateway stopped while Git ran.")
        XCTAssertFalse(harness.model.canMutate, "an uncertain outcome is read before anything else starts")
        let reads = harness.reads
        await harness.model.acknowledgeOutcome()
        XCTAssertEqual(harness.reads, reads + 1, "acknowledging an uncertain outcome rereads the repository")
        XCTAssertTrue(harness.model.canMutate)
    }

    func testAPreAcceptanceRefusalRanNothing() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.replies = [.failure(LatchError.git(code: "invalid_request", reason: "That request is malformed."))]
        await harness.model.applyToSection(.changes)
        guard case .refused(_, let code, _) = harness.model.mutations.phase else { return XCTFail() }
        XCTAssertEqual(code, "invalid_request")
        XCTAssertTrue(harness.model.canMutate)
    }

    func testAnOutcomeThatCouldNotBeDeliveredMustBeReconciled() async throws {
        for code in ["receipt_unavailable", "response_too_large"] {
            let harness = Harness()
            await harness.model.refresh()
            harness.replies = [.failure(LatchError.git(code: code, reason: "Outcome unavailable."))]
            await harness.model.applyToSection(.changes)
            guard case .lost = harness.model.mutations.phase else { return XCTFail("\(code): \(harness.model.mutations.phase)") }
            XCTAssertFalse(harness.model.canMutate)
        }
    }

    func testMutationsWaitForTheLinkAndControl() async throws {
        let harness = Harness()
        await harness.model.refresh()
        harness.online = false
        XCTAssertTrue(harness.model.isStale)
        XCTAssertFalse(harness.model.canMutate)
        XCTAssertTrue(harness.model.mutationBlockReason?.hasPrefix("Reconnecting") == true)
        await harness.model.applyToSection(.changes)
        XCTAssertTrue(harness.sent.isEmpty)

        harness.online = true
        harness.access = false
        XCTAssertTrue(harness.model.mutationBlockReason?.contains("can't change this repository") == true)
        await harness.model.applyToSection(.changes)
        XCTAssertTrue(harness.sent.isEmpty)
    }

    func testCommitReviewFollowsTheIndexAndTheWorktree() async throws {
        let harness = Harness(statusJSON: Self.status(staged: false))
        await harness.model.refresh()
        XCTAssertFalse(harness.model.canCommit)
        XCTAssertEqual(harness.model.commitBlockReason, "Stage the files to include first.")

        let busy = Harness(statusJSON: Self.status(operation: #"{"kind":"merge","step":null,"total":null}"#))
        await busy.model.refresh()
        XCTAssertFalse(busy.model.canCommit)
        XCTAssertTrue(busy.model.commitBlockReason?.contains("merge or rebase") == true)

        let ready = Harness()
        await ready.model.refresh()
        XCTAssertTrue(ready.model.canCommit)
        XCTAssertEqual(ready.model.excludedFileCount, 3)
        XCTAssertEqual(GitCommitMessage.exclusionNote(3), "3 unstaged files won't be included.")
        XCTAssertEqual(GitCommitMessage.buttonTitle(fileCount: 1, branch: "main"), "Commit 1 file to main")
        XCTAssertEqual(GitCommitMessage.buttonTitle(fileCount: 2, branch: nil), "Commit 2 files to detached HEAD")
        XCTAssertEqual(
            GitCommitMessage.activeSessionsWarning(ready.model.status?.sessions ?? []),
            "Agent is active in this worktree. If files change before this runs, Latch will ask you to review again."
        )
        XCTAssertFalse(ready.model.offersStaging(try XCTUnwrap(ready.model.status?.entries[3])), "conflicts resolve on the Mac")
        XCTAssertFalse(ready.model.offersSectionAction(.conflicted))
    }

    func testATruncatedFileListCannotBeCommitted() async throws {
        let harness = Harness(statusJSON: Self.status().replacingOccurrences(of: "\"entriesTruncated\":false", with: "\"entriesTruncated\":true"))
        await harness.model.refresh()
        XCTAssertFalse(harness.model.canCommit)
        XCTAssertTrue(harness.model.commitBlockReason?.contains("cannot show every changed file") == true)
        await harness.model.commit(summary: "Hidden files", description: "")
        XCTAssertTrue(harness.sent.isEmpty)
    }

    func testBranchNamePlausibility() {
        XCTAssertTrue(GitBranchName.isPlausible("feature/git-ui"))
        for bad in ["", "-x", "a..b", "a b", "a~1", "x.lock", "a/", "@", "a@{b", "a:b", "x\u{7}"] {
            XCTAssertFalse(GitBranchName.isPlausible(bad), bad)
        }
    }

    // MARK: Fixtures

    nonisolated static let allKeys = #","gitBranches":true,"gitStage":true,"gitUnstage":true,"gitCommit":true,"gitCreateBranch":true,"gitSwitchBranch":true,"gitReceipts":true"#

    nonisolated static let branches = """
    {"root":"/r","head":{"branch":"main","oid":"\(head)","detached":false},
     "branches":[
      {"name":"main","oid":"\(head)","current":true,"upstream":null,"ahead":null,"behind":null,
       "upstreamGone":false,"subject":"Init","committedAt":null,"checkedOutElsewhere":false},
      {"name":"feature","oid":"\(String(repeating: "c", count: 40))","current":false,"upstream":"origin/feature",
       "ahead":2,"behind":1,"upstreamGone":false,"subject":"Work","committedAt":"2026-10-12T09:00:00Z",
       "checkedOutElsewhere":false},
      {"name":"elsewhere","oid":"\(String(repeating: "d", count: 40))","current":false,"upstream":"origin/old",
       "ahead":null,"behind":null,"upstreamGone":true,"subject":"Old","committedAt":null,"checkedOutElsewhere":true}
     ],
     "truncated":false,
     "preconditions":{"branch":"main","head":"\(head)","index":"\(index)","worktree":"\(worktree)"}}
    """

    nonisolated static func capabilities(_ gitKeys: String) -> String {
        """
        {"protocolVersion":2,"productVersion":"2.0.0",
         "capabilities":{"create":true,"openViewer":true,"localAttach":true,
          "cloudAttach":false,"selfUpdate":true,"extensions":[]},
         "endpoints":{"sessions":true,"preview":true,"terminal":true,"conversation":true,
          "browseDirectories":true,"createSession":true,
          "gitRepositories":true,"gitResolve":true,"gitStatus":true,"gitDiff":true\(gitKeys)},
         "features":{"exclusiveTerminal":true},"gatewayInstanceId":"gw-a-b",
         "operationRetentionSeconds":600}
        """
    }

    private func makePairedModel() -> AppModel {
        AppModel(
            pairedGatewayFactory: { _ in
                LatchGateway(
                    link: try GatewayLink(address: "https://mac.local:8787", token: "token"),
                    session: StubProtocol.session()
                )
            },
            newSessionFolderStore: MemoryNewSessionFolderStore(),
            terminalUnlock: TerminalUnlock(authenticator: StubDeviceOwnerAuthenticator(), grace: 600)
        )
    }

    private func pairedRecord() -> PairedDeviceRecord {
        PairedDeviceRecord(
            deviceId: "phone",
            name: "Phone",
            devicePublicKey: String(repeating: "11", count: 32),
            mac: PairedMac(deviceId: "mac", publicKey: String(repeating: "22", count: 32), name: "Mac"),
            permission: .control,
            comparison: "0123 4567 89ab cdef",
            controlPlane: URL(string: "https://control.example")!
        )
    }
}

/// A repository model whose Mac is scripted: each mutation and receipt read
/// takes the next scripted reply.
@MainActor
private final class Harness {
    enum Reply {
        case receipt(String)
        case failure(Error)
    }

    var replies: [Reply] = []
    var lookups: [Reply] = []
    var sent: [GitMutationAction] = []
    var lookedUp: [String] = []
    var reads = 0
    var branchReads = 0
    var sleeps = 0
    var online = true
    var access = true
    var ids = 0
    lazy var model: GitRepositoryModel = makeModel()
    private let statusValue: GitStatus

    init(status: GitStatus) {
        statusValue = status
    }

    convenience init(statusJSON: String = GitMutationTests.status()) {
        // swiftlint:disable:next force_try
        self.init(status: try! JSONDecoder().decode(GitStatus.self, from: Data(statusJSON.utf8)))
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
                return statusValue
            },
            readDiff: { _, _, _, _ in throw LatchError.transport("unused") },
            hasAccess: { [unowned self] in access },
            isOnline: { [unowned self] in online },
            support: .all,
            mutate: { [unowned self] action in
                sent.append(action)
                return try next(&replies)
            },
            lookupReceipt: { [unowned self] id in
                lookedUp.append(id)
                return try next(&lookups)
            },
            readBranches: { [unowned self] _ in
                branchReads += 1
                return try JSONDecoder().decode(GitBranchList.self, from: Data(GitMutationTests.branches.utf8))
            },
            makeRequestID: { [unowned self] in
                ids += 1
                return "00000000-0000-0000-0000-\(String(format: "%012d", ids))"
            },
            sleep: { [unowned self] _ in sleeps += 1 }
        )
    }
}
