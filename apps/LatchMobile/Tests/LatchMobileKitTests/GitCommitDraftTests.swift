import XCTest
@testable import LatchMobileKit

@MainActor
final class GitCommitDraftTests: XCTestCase {
    func testPopulatesEditableFieldsAndKeepsOriginalSourceWhenIndexChanges() async throws {
        var status = try JSONDecoder().decode(GitStatus.self, from: Data(GitMutationTests.status().utf8))
        let repository = GitRepositoryModel(
            folder: "/r", read: { _ in status }, readDiff: { _, _, _, _ in fatalError("not used") },
            support: GitMutationSupport(commit: true),
            mutate: { _ in fatalError("generation must not commit") },
            lookupReceipt: { _ in fatalError("generation has no receipt") }
        )
        await repository.refresh()
        let model = GitCommitDraftModel(
            repository: repository, supported: { true }, readiness: { .ready },
            generate: { Self.response($0) }, cancel: { _ in }
        )
        model.startGeneration()
        try await settled(model)
        XCTAssertEqual(model.summary, "Improve the picker")
        XCTAssertEqual(model.details, "Keep selection visible.")
        XCTAssertFalse(model.requiresReview)
        XCTAssertNotNil(model.coverageNote)
        model.editSummary("Improve selection")
        XCTAssertEqual(model.summary, "Improve selection")
        status.preconditions?.index = "new-index"
        await repository.refresh()
        model.sourceDidChange()
        XCTAssertTrue(model.requiresReview)
        XCTAssertFalse(model.canCommitMessage)
        XCTAssertEqual(model.result?.source.index, GitMutationTests.index)
        model.acknowledgeManualReview()
        XCTAssertFalse(model.requiresReview)
        XCTAssertEqual(model.result?.source.index, GitMutationTests.index, "manual review must not rewrite generated source")
    }

    func testLateResponseNeverOverwritesNewerTyping() async throws {
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(GitMutationTests.status().utf8))
        let repository = GitRepositoryModel(
            folder: "/r", read: { _ in status }, readDiff: { _, _, _, _ in fatalError("not used") },
            support: GitMutationSupport(commit: true), mutate: { _ in fatalError("not used") },
            lookupReceipt: { _ in fatalError("not used") }
        )
        await repository.refresh()
        var pending: CheckedContinuation<GitCommitMessageResponse, Never>?
        var request: GitCommitMessageRequest?
        let started = expectation(description: "request started")
        let model = GitCommitDraftModel(
            repository: repository, supported: { true }, readiness: { .ready },
            generate: { value in
                request = value
                return await withCheckedContinuation { pending = $0; started.fulfill() }
            }, cancel: { _ in }
        )
        model.startGeneration()
        await fulfillment(of: [started], timeout: 2)
        model.editSummary("My newer message")
        model.editDetails("My explanation")
        pending?.resume(returning: Self.response(try XCTUnwrap(request)))
        try await settled(model)
        XCTAssertEqual(model.summary, "My newer message")
        XCTAssertEqual(model.details, "My explanation")
        XCTAssertNil(model.result)
        XCTAssertTrue(model.notice?.contains("newer typing") == true)
        model.dismiss()
        XCTAssertFalse(model.canGenerate)
    }

    func testCancelledAndDismissedResponsesCannotPopulateOrReplaceLaterGeneration() async throws {
        let fixture = try Fixture()
        let model = await fixture.model()
        model.startGeneration()
        await fixture.waitForRequests(1)
        model.startGeneration()
        XCTAssertEqual(fixture.requests.count, 1, "repeated taps admit one generation")
        let first = fixture.requests[0]
        model.cancelGeneration()
        model.startGeneration()
        await fixture.waitForRequests(2)
        fixture.pending[0].resume(returning: Self.response(first))
        await Task.yield()
        XCTAssertTrue(model.isGenerating, "old completion must not finish the new request")
        XCTAssertEqual(model.summary, "")
        fixture.pending[1].resume(returning: Self.response(fixture.requests[1]))
        try await settled(model)
        XCTAssertEqual(model.summary, "Improve the picker")
        model.startGeneration()
        await fixture.waitForRequests(3)
        model.dismiss()
        var late = Self.response(fixture.requests[2])
        late.summary = "Replace after dismissal"
        fixture.pending[2].resume(returning: late)
        for _ in 0..<10 { await Task.yield() }
        XCTAssertEqual(model.summary, "Improve the picker")
        XCTAssertFalse(model.canCommitMessage)
        XCTAssertFalse(model.canGenerate)
    }

    func testRefreshRepositoryAndResponseIdentityChangesDiscardPendingDrafts() async throws {
        for change in ["branch", "head", "index", "repository", "request", "response-source", "refresh-failure", "revoked", "offline"] {
            let fixture = try Fixture()
            let model = await fixture.model()
            model.editSummary("Manual fallback")
            model.startGeneration()
            await fixture.waitForRequests(1)
            var response = Self.response(fixture.requests[0])
            switch change {
            case "branch": fixture.status.preconditions?.branch = "another"
            case "head": fixture.status.preconditions?.head = String(repeating: "b", count: 40)
            case "index": fixture.status.preconditions?.index = "another-index"
            case "repository": fixture.status.root = "/another-repository"
            case "request": response.requestId = UUID().uuidString
            case "response-source": response.source.index = "wrong-index"
            case "refresh-failure": fixture.failRefresh = true
            case "revoked": fixture.access = false
            case "offline": fixture.online = false
            default: break
            }
            fixture.pending[0].resume(returning: response)
            try await settled(model)
            XCTAssertNil(model.result, change)
            XCTAssertEqual(model.summary, "Manual fallback", change)
            XCTAssertNotNil(model.notice, change)
        }
    }

    func testOldGatewayReadinessOfflineAndRevokedAccessPreserveManualFallback() async throws {
        let fixture = try Fixture()
        let model = await fixture.model()
        for ready in [GitDraftReadinessState.unconfigured, .unavailable] {
            fixture.ready = ready
            XCTAssertFalse(model.canGenerate)
            XCTAssertNotNil(model.availabilityNote)
            XCTAssertTrue(model.canCommitMessage)
        }
        fixture.ready = .ready
        fixture.supported = false
        XCTAssertFalse(model.canGenerate)
        XCTAssertTrue(model.canCommitMessage)
        fixture.supported = true
        fixture.online = false
        XCTAssertFalse(model.canGenerate)
        fixture.online = true
        fixture.access = false
        XCTAssertFalse(model.canGenerate)
        model.startGeneration()
        XCTAssertTrue(fixture.requests.isEmpty)
        model.editSummary("Manual fallback")
        XCTAssertEqual(model.summary, "Manual fallback")
    }

    func testTypedFailuresLeaveTextEditableAndAllowAnotherAttempt() async throws {
        for code in [GitDraftFailureCode.unconfigured, .unauthorized, .rateLimited, .timedOut, .providerRefusal, .providerError, .staleSource, .cancelled, .noStagedChanges, .unusableContext] {
            let fixture = try Fixture()
            let model = await fixture.model(error: GitCommitDraftError(failure: GitCommitMessageFailure(
                requestId: nil, code: code, message: "raw details are not UI copy", retryAfterSeconds: 17
            )))
            model.editSummary("My manual message")
            model.editDetails("My manual description")
            model.startGeneration()
            try await settled(model)
            XCTAssertEqual(model.summary, "My manual message", "\(code)")
            XCTAssertEqual(model.details, "My manual description", "\(code)")
            XCTAssertNil(model.result)
            XCTAssertTrue(model.canCommitMessage)
            XCTAssertTrue(model.canGenerate)
            XCTAssertNotNil(model.notice)
            XCTAssertFalse(model.notice?.contains("raw details") == true)
        }
    }

    func testEveryFurtherSourceChangeRequiresFreshManualReview() async throws {
        let fixture = try Fixture()
        let model = await fixture.model()
        model.startGeneration()
        await fixture.waitForRequests(1)
        fixture.pending[0].resume(returning: Self.response(fixture.requests[0]))
        try await settled(model)
        let original = model.result?.source
        for index in ["second-index", "third-index"] {
            fixture.status.preconditions?.index = index
            await fixture.repository?.refresh()
            model.sourceDidChange()
            XCTAssertTrue(model.requiresReview)
            model.acknowledgeManualReview()
            XCTAssertFalse(model.requiresReview)
            XCTAssertEqual(model.result?.source, original)
        }
    }

    @MainActor
    private final class Fixture {
        var status: GitStatus
        var access = true
        var online = true
        var supported = true
        var ready: GitDraftReadinessState = .ready
        var failRefresh = false
        var repository: GitRepositoryModel?
        var requests: [GitCommitMessageRequest] = []
        var pending: [CheckedContinuation<GitCommitMessageResponse, Never>] = []

        init() throws {
            status = try JSONDecoder().decode(GitStatus.self, from: Data(GitMutationTests.status().utf8))
        }

        func model(error: Error? = nil) async -> GitCommitDraftModel {
            let repository = GitRepositoryModel(
                folder: "/r", read: { [self] _ in
                    if failRefresh { throw URLError(.notConnectedToInternet) }
                    return status
                }, readDiff: { _, _, _, _ in fatalError("drafts must not use mobile diffs") },
                hasAccess: { [self] in access }, isOnline: { [self] in online },
                support: GitMutationSupport(commit: true),
                mutate: { _ in fatalError("drafts must not mutate Git") },
                lookupReceipt: { _ in fatalError("drafts have no receipt") }
            )
            self.repository = repository
            await repository.refresh()
            return GitCommitDraftModel(
                repository: repository, supported: { [self] in supported }, readiness: { [self] in ready },
                generate: { [self] request in
                    if let error { throw error }
                    requests.append(request)
                    return await withCheckedContinuation { pending.append($0) }
                }, cancel: { _ in }
            )
        }

        func waitForRequests(_ count: Int) async {
            let deadline = ContinuousClock.now.advanced(by: .seconds(2))
            while requests.count < count, ContinuousClock.now < deadline { await Task.yield() }
            XCTAssertEqual(requests.count, count)
        }
    }

    private func settled(_ model: GitCommitDraftModel) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while model.isGenerating, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(1))
        }
        XCTAssertFalse(model.isGenerating)
    }

    private static func response(_ request: GitCommitMessageRequest) -> GitCommitMessageResponse {
        GitCommitMessageResponse(
            requestId: request.requestId, summary: "Improve the picker", body: "Keep selection visible.",
            source: GitDraftSource(
                root: request.path, branch: request.expected.branch, head: request.expected.head,
                index: request.expected.index, capturedIndex: "tree"
            ),
            coverage: GitDraftCoverage(
                totalFiles: 2, includedFiles: 2, patchFiles: 1, patchBytes: 80,
                omissions: [GitDraftOmission(path: "image.png", reason: .binary)]
            )
        )
    }
}
