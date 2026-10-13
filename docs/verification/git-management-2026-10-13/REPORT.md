# Git management independent verification — 2026-10-13

Mission: `coo:1149`, objective: `coo:1149.1tsa`.

The four Git slices have substantial automated and live coverage, and this verification fixed confirmed defects in commit review, retry safety, reconnects, session grouping, pagination, response limits and accessibility layout. **This is not a release approval.** The full workspace check currently fails in an unrelated terminal snapshot property test, and the unexercised physical/paired and interactive mobile flows below remain release gates.

All Git fixtures were created under `/tmp`: disposable working repositories, local bare remotes, isolated Latch homes and generated test tokens. No production remote, credential or user repository was used for Git operations. Existing checkout changes were preserved. Nothing was staged or committed. Temporary app harness code was removed; its source is retained as a noncompiled reproduction artifact.

## Results and reproducible commands

Run from the checkout root unless a directory is specified.

| Command | Result |
| --- | --- |
| `cargo test -p latch cli::serve::git:: --lib` | **PASS:** 118 tests; actual temporary Git repositories, local bare remotes, subprocess deadlines and restart fixtures |
| `just check` | **FAIL:** fmt and clippy pass; latch library 395 pass / 2 ignored; later `latch-term` snapshot property test fails. The full command stops there. |
| `cargo test -p latch-term --test snapshot_roundtrip arbitrary_streams_round_trip` | **FAIL:** independently reproduces the terminal failure in 0.36 s; no Git feature path is involved |
| `just check-web` | **PASS:** typecheck/build and 19 tests; run separately because the workspace failure prevents `just check` from reaching it |
| `cd apps/LatchMobile && swift test` | **PASS:** 427 tests, 1 skipped, 0 failures |
| `python3 scripts/generate-remote-access-types.py --check` | **PASS:** Rust and TypeScript contract copies current |
| `python3 apps/LatchMobile/Tools/generate-contract.py --check` | **PASS:** vendored Swift contract current |
| `git diff --check` | **PASS** |
| Normal app build shown below | **PASS:** temporary harness removed |
| `cargo build -p latch` then `python3 docs/verification/git-management-2026-10-13/live_gateway.py` | **PASS:** 70 requests (largest response 1,819 bytes); per-route results in [live-gateway.json](live-gateway.json); includes real HTTP and real Git, two device identities and a live session-directory change |

Normal app build:

```sh
xcodebuild -project apps/LatchMobile/App/LatchMobile.xcodeproj \
  -scheme LatchMobile -configuration Debug \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /tmp/LatchGitVerificationHarness \
  CODE_SIGNING_ALLOWED=NO build
```

The first final-check attempt exhausted disk space while creating a second DerivedData directory and the disposable simulator. Only this verification's failed cache and disposable simulator were removed; subsequent builds succeeded with the existing verification cache. This infrastructure failure is separate from the reproduced terminal test failure.

## Defects fixed

| Finding | Final behavior and regression reference |
| --- | --- |
| Settled receipts were deleted after 512 requests, allowing an old ID to run again | Accepted IDs are retained. `mutation_tests::old_request_ids_remain_deduplicated_after_more_than_512_new_receipts` |
| Reconciliation queried a remote's fetch URL, even with a separate push URL | Checks query the reviewed push URL and refuse a changed address. `push_tests::reconciliation_checks_the_push_url_not_the_fetch_url`, `changed_push_url_is_not_used_to_reconcile_an_old_push` |
| One reviewed remote could have multiple configured push URLs | Preview blocks it and execution refuses it. `multiple_push_urls_cannot_send_to_an_unreviewed_destination` |
| A nonzero Git exit without a destination report could be called a definite refusal | Ambiguous transport/hook outcomes remain uncertain. `a_lost_remote_report_is_uncertain_even_when_git_exits_nonzero` |
| Local tracking refs or negative remote observations could justify success or a new push | Started, interrupted pushes stay uncertain until explicit remote success evidence. A pre-start receipt is the only `not_applied` proof. Updated push restart/reconciliation tests cover this. |
| A missing PID record was treated as proof Git stopped | Missing/malformed records are unknown; all mutations record process groups. `a_missing_process_record_does_not_prove_git_stopped` |
| A Mac without reconciliation could forget an uncertain push when its result was closed | The runner retains the unresolved push and blocks another push. `GitPushTests.testOlderMacsHideReviewPushAndCheckTheRemote` |
| A failed status refresh could leave cached state usable | Cached status remains stale and mutations remain blocked until a fresh response. `GitManagementTests.testAFailedRefreshAndAnOfflineLinkKeepTheLastStatusReadable` |
| Mutation response/storage errors were treated as proof nothing ran | `response_too_large` and `receipt_unavailable` trigger receipt reconciliation. `GitMutationTests.testAnOutcomeThatCouldNotBeDeliveredMustBeReconciled` |
| Sync pages only bound upstream, despite HEAD also defining the incoming range | Cursors bind both commits and page a pinned range. `integrate_tests::sync_pages_incoming_commits_and_refuses_a_cursor_once_upstream_moves` |
| Unit separators in commit text could remove commits from a review | Log fields use NUL separators. `commit_text_delimiters_do_not_hide_outgoing_or_incoming_commits` |
| Per-field bounds did not guarantee the final JSON response size | Every Git handler uses the serialized JSON guard below 512 KiB, including escaping. `http::tests::git_wire_bound_counts_json_escaping_and_never_sends_a_512_kib_payload` |
| Fixed footers could cover the entire review at accessibility text sizes | Commit, Sync and Push action areas scroll with content; full button labels wrap. Commit avoids forced keyboard focus at accessibility sizes. Screenshots below. |
| Open screens captured the original, closed gateway adapter | Calls resolve the current gateway, bound to the original Mac identity. Models/receipt IDs survive back-navigation in memory. `GitManagementTests.testAnOpenRepositoryUsesTheReplacementGatewayAfterReconnect` |
| Session mapping used launch cwd after a shell changed directory | Git grouping reads the authenticated kernel child's current cwd on macOS/Linux. `session::tests::the_session_childs_current_directory_can_differ_from_its_launch_directory`; live HTTP harness also verifies mapping and removal after stop. |
| Commit hooks could stage unreviewed content | Hooks run against a private index. Forwarders check the reviewed tree after pre-commit and message hooks; changed staging refuses the commit without touching HEAD or the real index. Tests cover pre-commit, prepare-commit-msg, commit-msg and working-file edits without staging. |
| Large untracked folders had only partial fingerprints | Selected/all staging rejects folders over the bounded 4,096-entry walk. `a_partially_fingerprinted_untracked_folder_cannot_be_staged` |
| A commit could include staged files omitted from a bounded status list | Gateway and phone refuse commits from truncated reviews. `a_commit_cannot_include_files_omitted_from_the_phone_review`, `GitMutationTests.testATruncatedFileListCannotBeCommitted` |

Test modules live in `crates/latch/src/cli/serve/git/{tests,mutation_tests,integrate_tests,push_tests}.rs`, `crates/latch/src/cli/serve/http/tests.rs`, `crates/latch/src/cli/remote_access/tests.rs` and `apps/LatchMobile/Tests/LatchMobileKitTests/Git*Tests.swift`.

## Requirement matrix

**PASS-A** means automated evidence for the named path, **PASS-L** real loopback HTTP/Git, and **PASS-UI** inspected simulator rendering with live gateway data. **NOT RUN** never means pass. A code limitation or known failure is explicitly marked **FAIL**.

### 1. Routes, permissions and subprocess boundaries

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Every Git read/mutation/preview/receipt/reconcile route at Control | PASS-A | Shared route table plus router and paired-proxy route/grant tests enumerate all routes |
| Observe/Interact rejected before filesystem access | PASS-A, PASS-L | Router/proxy tests; live status and receipt calls return 403 |
| Cross-device receipt isolation and changed-argument reuse | PASS-A, PASS-L | Two device identities; other device gets 404; changed request body gets 409 |
| Revocation through a running encrypted paired link | NOT RUN | Proxy grant tests and existing device-revocation tests pass; no two physical paired phones were enrolled in this verification |
| No arbitrary Git arguments, path/ref/options injection | PASS-A | HTTP body validation and proxy rejection tests; literal pathspecs and fixed subprocess arrays |
| Canonical roots, symlinks, parent paths, linked worktrees, submodules | PASS-A | Worktree-resolution fixtures, including `.git` files |
| Deleted/nonrepo/bare/Git-directory targets rejected | PASS-A | Resolution and deleted-after-open tests |
| Replaced valid repository during an operation | NOT RUN | Canonical worktree is resolved again under the mutation lock; no full adversarial replacement race was injected |
| Noninteractive credentials/editors/pagers | PASS-A | Environment assertions, fake SSH/HTTP credentials and failing signing/hook fixtures; no real credentials |
| Deadlines, bounded subprocess output, process group kill | PASS-A | Hanging hook/fetch/push and output-limit tests |
| JSON payloads below 512 KiB | PASS-A, PASS-L | Final serialized guard; every live response measured below the bound |
| Hook forwarding introduces no phone-controlled shell text | PASS-A / source | Only fixed scripts and literally quoted Mac-derived paths/hex trees are used. Phone messages/arguments remain structured Git arguments. |

### 2. Read-only repositories and diffs

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Porcelain v2 NUL parsing, rename/original paths, conflicts | PASS-A | Raw parser fixtures and real rename/conflict repositories |
| Spaces/newlines/quotes/leading dashes/Unicode/non-UTF-8 names | PASS-A; UTF-8 variants PASS-L | Filename-byte keys round-trip; non-UTF-8 test runs where filesystem permits |
| Live copy detection | NOT RUN | Ordinary/rename records are parsed; no real Git copy-status fixture was exercised |
| Staged/unstaged/untracked, detached HEAD, unborn/no upstream | PASS-A | Status fixtures; staged/unstaged/untracked also exercised live |
| Ahead/behind, merge/rebase state, FETCH_HEAD time | PASS-A, PASS-L | Local refs; real fetch/divergence/merge conflict; rebase state automated |
| Diff pagination returns each hunk once and rejects stale cursors | PASS-A | Large multi-hunk fixtures and phone append/reload state tests |
| Binary/oversized/folder/submodule explanations | PASS-A | Real Git fixtures; live text diff rendered in simulator |
| Shared Mac recents, no phone persistence | PASS-A, PASS-L | Open on phone-a, list on phone-b; phone list model stores no recents |
| Session start/stop/activity/current-directory mapping | PASS-A, PASS-L | Live active/idle sessions shown in UI; final harness verifies a changed child cwd and removal after stop |
| Opening/status/branches/sync/push preview never fetch | PASS-A, PASS-L | Unfetched remote changes stay absent until explicit fetch |

### 3. Stage, commit, branches and receipts

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Selected and all-file stage/unstage; rename/delete/untracked/unusual names | PASS-A, PASS-L | Real temporary Git operations and live mobile model staging |
| Commit reviewed index; unstaged work preserved | PASS-A, PASS-L | Committed blob checked separately from later disk contents |
| Hooks cannot expand reviewed commit; identity/signing/hook failure | PASS-A, PASS-L | Private-index regression fixtures and live pre-commit staging refusal |
| Branch list/create/switch and would-overwrite refusal | PASS-A, PASS-L | Disposable branches, borrowed worktree branch and overwrite fixtures |
| No automatic stash/discard/reset | PASS-A | Refusal fixtures preserve work; autostash config explicitly challenged for updates |
| Stale HEAD/index/worktree/relevant refs and concurrent devices | PASS-A, PASS-L | File/index/ref edits, two-client serialized commits; live stale stage refusal |
| Partial folder fingerprint / truncated commit review | PASS-A | Mutations fail closed at inspection limits |
| Durable IDs, duplicate replay, changed arguments, receipt restart | PASS-A, PASS-L | Retention over 512 receipts, replay/device tests and abandoned-receipt fixtures |
| Gateway dies while Git survives; another gateway/device starts new work | NOT RUN | Recovery now recognizes live/missing process records. Cross-gateway mutation exclusion while an orphan survives was not exercised end to end; see release gates. |
| Phone process termination restores pending request IDs | FAIL (source limitation) | IDs and runner state survive navigation/reconnect within the app lifetime, but are not persisted across phone app termination |

### 4. Fetch, update, merge/rebase and operation recovery

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Explicit fetch and no implicit network | PASS-A, PASS-L | Fetch time and upstream change only after explicit command |
| Fast-forward default, divergence refusal, no fallback | PASS-A, PASS-L | Real diverged repos, `not_fast_forward`, unchanged branch |
| Successful explicit merge/rebase | PASS-A | Disposable local remotes; merge also exercised live |
| Real merge/rebase conflicts; resolved staging/Continue | PASS-A | Live merge marker refusal, Mac-side resolution, stage and Continue; rebase automated |
| Rebase stop without unmerged entries | PASS-A | Edit-stop tests exercise Continue and Git's refusal |
| Abort confirmation and current-operation checks | PASS-A | Merge/rebase Abort fixtures; stale state checks |
| Progress/timeouts; closed connection/Stop waiting do not cancel | PASS-A | Detached task, progress and timeout tests; mobile runner polling and Stop waiting state tests |
| Pending/rejected/completed/uncertain recovery after restart | PASS-A | Durable receipt fixture tests; not a physical-device reconnect walkthrough |
| Phone never claims to edit conflicts | PASS-UI / source | Conflict screen says resolve on Mac; staged resolution tested separately |

### 5. Push review and reconciliation

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Source/destination names/SHAs, remote URL, default/new branch | PASS-A, PASS-UI | Offline preview and review rendering; default/non-default fixtures |
| Complete bounded outgoing list; unusual commit text | PASS-A | 50-commit pages, stale source/destination cursor tests and NUL log-field regression |
| No-upstream explicit destination selection | PASS-A | Gateway and phone selection tests; no silent destination or upstream setting |
| Normal push, stale source/URL/tracking ref, changed remote | PASS-A, PASS-L | Local bare remote; non-fast-forward and protected remote rejection fixtures |
| Noninteractive credentials/pre-push hook failure | PASS-A | Fake SSH/HTTP only; no production credentials. A no-report hook result remains uncertain. |
| Lost response before start / after acceptance, replay/restart | PASS-A | Pre-start receipt can prove not run; remote-success evidence settles started requests |
| Unknown/absent/unchanged remote never permits blind retry | PASS-A | Updated uncertainty tests; missing PID/configured pushurl cases |
| Force/reset/clean/branch-delete APIs absent | PASS-A / source | Shared route/proxy rejection tests and typed mutations expose none |
| Push execution and receipt reconciliation via simulator UI taps | NOT RUN | Preview rendered against live gateway; actual push exercised over live HTTP and phone runner tests, not UI-driven tap automation |

### 6. Mobile flows, accessibility and compatibility

| Requirement | Status | Evidence / limit |
| --- | --- | --- |
| Repositories/repository/diff/Commit/Branches/Sync/Push/conflict screens | PASS-UI | Real SwiftUI views and live gateway data through the temporary direct-loopback harness |
| Staging and commit through mobile models against real gateway | PASS-L | Harness `stage` / `completed` cases ran production model methods; HTTP harness independently checks committed/index/worktree state |
| Rejected/refresh-required/reconnecting rendering | PASS-UI | Real gateway refusals; stale harness sets link offline while retaining live state |
| Pending/uncertain/progress/Stop waiting visual walkthrough | NOT RUN | Model transitions tested; no live screenshots or UI interaction for all these states |
| Device-owner auth before Mac file data; no terminal attachment | PASS-A; physical NOT RUN | Authenticator stub denies all Git requests until approval; terminal attach counter remains zero. Screenshot harness bypasses pairing/owner auth deliberately. |
| Missing Control, offline mutation blocking, older/per-operation gating | PASS-A | GitManagement/Mutation/Sync/Push tests; no older physical Mac build installed |
| VoiceOver labels and 44pt staging targets | PASS-A / source | Spoken-status tests plus source inspection; no live VoiceOver audio/focus traversal or measured hit-test automation |
| Previous/next file navigation | PASS-A, PASS-UI | Neighbor selection tested; both controls rendered in diff screenshot; taps not automated |
| Active-session warnings | PASS-UI | Real active and idle fixture sessions; Commit/Sync/Push warnings rendered |
| Light/dark and largest accessibility Dynamic Type | PASS-UI (initial viewport) | Screens below. Fixed-footer occlusion corrected and rechecked. Full interactive scroll/focus traversal remains NOT RUN. |

## Simulator evidence

iOS 27 / iPhone 17. The temporary harness uses a direct loopback `LatchGateway` and production models/views. It does not establish an encrypted paired link, exercise Face ID or simulate a physical network interruption. These screenshots therefore prove rendering and the explicitly scripted model operations, not the entire production connection flow.

| Screen | Light | Dark, accessibility-extra-extra-extra-large |
| --- | --- | --- |
| Repository / active and idle sessions | [image](repository-light.png) | [image](repository-dark-ax5.png) |
| Repositories sheet | [image](repositories-light.png) | [image](repositories-dark-ax5.png) |
| Numbered unified diff | [image](diff-light.png) | [image](diff-dark-ax5.png) |
| Commit | [image](commit-light.png) | [image](commit-dark-ax5.png) |
| Branches | [image](branches-light.png) | [image](branches-dark-ax5.png) |
| Sync / divergence | [image](sync-light.png) | [image](sync-dark-ax5.png) |
| Push review | [image](push-light.png) | [image](push-dark-ax5.png) |
| Real merge conflict | [image](conflicts-light.png) | [image](conflicts-dark-ax5.png) |
| Readable stale state | [image](stale-light.png) | [image](stale-dark-ax5.png) |
| Git rejection | [image](rejected-light.png) | [image](rejected-dark-ax5.png) |
| Refresh required | [image](refresh-required-light.png) | [image](refresh-required-dark-ax5.png) |

The commit screenshot at maximum text size shows zero staged files after the scripted mobile commit; the light commit screenshot was taken before that mutation. The `completed-*` captures mount the repository after the operation, so they are not evidence of the transient completed toast.

### Reproduce screenshots

1. Run `live_gateway.py --keep`; use the printed runtime JSON path. It contains a disposable token; do not publish that file.
2. Temporarily append [simulator_harness.swift.txt](simulator_harness.swift.txt) to `LatchMobileApp.swift`, use `VerificationEntryView()` at its root, and skip the pairing task when `LATCH_VERIFY_SCREEN` is set. Temporarily add the `verificationList` injection used by the Repositories case. The template describes these changes; none remains compiled in the delivered app.
3. Create/boot a disposable iPhone 17 simulator, build/install the Debug app, then run `python3 docs/verification/git-management-2026-10-13/capture_simulator.py <runtime.json> <simulator-id>`.
4. Remove the temporary entrypoint, pairing bypass and list injection, rebuild the normal app, then stop the disposable gateway/sessions and delete the disposable simulator.

## Release gates and remaining decisions

1. **Required full check is red.** `latch-term/tests/snapshot_roundtrip.rs::arbitrary_streams_round_trip` fails for a minimized stream involving a scroll region, Unicode, repeated alternate-screen entry, rendition and alternate-screen exit. Seed `5750d4dada91008983e995481a179193c0b7187359ddc2ad114afdeec6e5f79d` reproduces independently. The generated regression entry was retained. Terminal emulator implementation was not changed by this objective; repair belongs in separate terminal work.
2. **Pending phone actions are not durable across app termination.** Back-navigation and gateway reconnect now preserve the runner during the app lifetime. Before claiming restart-safe phone behavior, define and implement durable pending-request storage scoped to Mac/device identity, separate from the Mac-owned recent-repositories list.
3. **Complete the production connection and interaction gates.** Test two real paired device identities through an encrypted link, live grant revocation, physical owner authentication, and UI-driven Sync/merge/rebase/Continue/Abort/push plus lost-response/progress/uncertain flows. Run VoiceOver focus traversal and full largest-text scrolling. Automated state coverage and direct-loopback screenshots do not replace these tests.
4. **Cross-gateway orphan exclusion needs an adversarial runtime check.** Process tracking prevents abandoned local work from being misreported as not run. The per-worktree mutex is gateway-memory scoped; verify or add durable exclusion before allowing a second gateway/device to start new work while a prior Git/hook survives a gateway crash.
5. **Conservative uncertainty is intentional.** A negative remote check cannot prove a started push never ran. Such pushes may require investigation on the Mac; no new push is offered from that observation alone. Multiple push URLs are unsupported from a single-target review. Receipts are retained until a safe tombstone compactor is designed. Hook forwarding preserves configured hooks and arguments while isolating index changes; hooks relying on the exact original `GIT_INDEX_FILE` or `core.hooksPath` value need compatibility evaluation.

The ordinary external-process race between the last recheck and Git's own locks remains; Git is still the authority for overwrite/ref refusals. The changes do not introduce force operations, conflict editing, partial staging or branch deletion.
