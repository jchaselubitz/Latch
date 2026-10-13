# Feature: Git management from Latch Mobile

**Status:** all four delivery slices are implemented: read-only status and diffs (1); stage, unstage, commit and branches (2); fetch, update, merge, rebase, Continue and Abort (3); and push with review and reconciliation (4). See the *Implemented* sections below. Independent verification and hardening ran on 2026-10-13; see [the evidence matrix and remaining release gates](verification/git-management-2026-10-13/REPORT.md). The *Remaining proposals* section lists what is not built.

## Product outcome

A person connected to their Mac through Latch can inspect and manage a local Git
repository from their phone. They can see uncommitted changes, commit selected
work, change branches, bring in remote changes, handle an interrupted merge or
rebase, and push a reviewed branch to its remote. The repository and Git
credentials remain on the Mac.

This is a repository view, independent of any running Latch session. Opening a
repository does not attach to or take control of a terminal. A session's working
directory can provide a shortcut to its repository, while the existing folder
browser can locate one when no session is open.

## Experience

The repository screen shows the canonical worktree path, current branch,
upstream and remote, ahead/behind counts, and whether a merge or rebase is in
progress. Its Changes section lists staged, unstaged, untracked, renamed,
deleted, and conflicted files separately. Tapping a file opens a bounded diff
or a clear explanation when a text diff cannot be shown, such as for a binary
file. The user can refresh at any time; stale information is marked after a
connection interruption.

The main actions are:

- Stage or unstage selected files, then review the staged diff and commit with
  a message. The initial version stages whole files; partial hunk staging can
  follow when its selection and conflict behavior are designed.
- Create a branch or switch to an existing local branch. Show why Git refuses a
  switch when local changes would be overwritten. Never stash, discard, or
  reset changes automatically.
- Fetch remote updates, then choose a fast-forward pull, merge, or rebase as
  appropriate for the current branch. Show the source and destination refs
  before starting an operation.
- When a merge or rebase stops on conflicts, list the conflicted files and the
  operation in progress. After the user resolves and stages them, offer
  **Continue** or **Abort**. Conflict editing can initially happen through an
  existing Mac or terminal workflow; the phone should not claim to resolve a
  conflict it cannot edit.
- Push only after showing the remote, source branch, destination branch, and
  commits to be sent. Pushing to `main` uses the same explicit review. Force
  push, reset, clean, and branch deletion are outside the initial scope.

Each action reports what Git actually did and then refreshes repository state.
The interface distinguishes a rejected command, a completed command, and an
unknown outcome after a lost connection.

## Implementation recommendations

### Use the existing Mac gateway

Add structured Git endpoints to the existing protocol-major-2 gateway. The
phone sends a repository identity, an operation name, and typed arguments; the
Mac runs Git in that worktree and returns bounded, structured results. The
existing Remote Link, paired-device authorization, and loopback gateway carry
these calls. The control plane and relay do not need Git credentials or
repository contents.

Register every new endpoint in the shared gateway route and grant table so the
remote proxy and HTTP router enforce the same rules. Require the current
**Control** grant for the initial feature, including repository selection and
read operations, consistent with the existing directory browser. Advertise Git
endpoints in gateway capabilities so older Mac and phone builds simply omit the
feature. Add the corresponding schemas and regenerate the Rust, TypeScript,
and Swift contract copies.

The Mac should resolve a chosen folder to its Git worktree root and keep that
canonical worktree path as the operation target. Do not infer a repository from
an untrusted display name or assume that `.git` is a directory: linked
worktrees and submodules may use a `.git` file. Revalidate the target before
every operation. Start with repositories the Mac user can access and reject
bare repositories for this working-tree interface.

Run the installed Git executable as a subprocess with a fixed argument array,
an explicit working directory, bounded output, and a noninteractive
environment. Do not pass phone input through a shell, accept arbitrary Git
arguments, or allow credential prompts to hang a request. Git uses the Mac's
existing remote credentials and configuration; missing credentials are
reported to the user for setup on the Mac. Be aware that normal Git commands
may run repository hooks or configured helpers on the Mac.

For status, parse `git status --porcelain=v2 -z` rather than human-readable
output. Keep filenames as data, including spaces, newlines, and renames. Diff
responses need file and total byte limits, pagination or truncation, and binary
file handling. The phone's current JSON response limit is 512 KiB, so a full
repository diff cannot be one unbounded response.

### Protect operations from stale state and retries

Another terminal, agent, or Git client may change the worktree while it is
open on the phone. Include the branch, HEAD, index/worktree snapshot, and
relevant upstream refs observed by the phone with each mutation. Recheck them
on the Mac immediately before acting; return a refresh-required result if the
preconditions no longer hold. Serialize Latch-initiated mutations per worktree,
while still detecting external changes.

Give each mutation a device-scoped request ID and a durable receipt that the
phone can query after reconnecting. A lost response must not cause Latch to
blindly repeat a commit, merge, rebase, or push. The operation model should
report pending, completed, rejected, and uncertain outcomes. After an
uncertain outcome, read the receipt and current Git state before offering a
new action. Long-running network commands need progress and cancellation
behavior that does not mistake a closed phone connection for proof that the
Git process stopped.

Git remains the authority for whether an operation is valid. The UI can hide
actions that clearly do not apply, but the Mac must check the current state and
return Git's refusal without silently repairing it. Prefer `fetch` followed by
an explicit update choice; make fast-forward-only the default pull behavior.

## Suggested delivery order

1. Repository selection, status, branch/upstream display, and bounded file
   diffs. Confirm behavior with renamed files, binary files, worktrees, and
   paths containing unusual characters.
2. Stage, unstage, commit, and branch creation/switching, with state
   preconditions and operation receipts.
3. Fetch, fast-forward update, merge, and rebase, including in-progress and
   conflict states with Continue and Abort.
4. Push with destination and commit review, noninteractive credential errors,
   and lost-response reconciliation.

Before release, test grant enforcement through the paired proxy, stale-state
rejection, concurrent Mac edits, lost responses, conflict recovery, and a push
to a disposable remote. Keep these tests separate from the user's real
repositories.

## Implemented: slice 2 (stage, unstage, commit, branches)

All routes sit at the **Control** grant in the shared route table
(`crates/latch/src/cli/serve/routes.rs`), so the loopback router and the
paired proxy enforce them identically. Each is advertised by its own
`endpoints` key, and a phone shows only what the Mac advertises.

| Route | Key | Purpose |
| --- | --- | --- |
| `GET /v2/git/branches?path=` | `gitBranches` | Local branches (current first, at most 500), upstream and ahead/behind as last fetched, `checkedOutElsewhere`, plus preconditions |
| `POST /v2/git/stage` | `gitStage` | Whole files (`files`, at most 200) or every entry in `all: ["unstaged"]` / `["untracked"]` |
| `POST /v2/git/unstage` | `gitUnstage` | Whole staged files (a rename restores both paths) or `all: ["staged"]` |
| `POST /v2/git/commit` | `gitCommit` | Commit exactly the reviewed index with the Mac's identity, hooks and signing |
| `POST /v2/git/branches/create` | `gitCreateBranch` | Create a branch at a reviewed commit, optionally switching to it |
| `POST /v2/git/branches/switch` | `gitSwitchBranch` | Switch to a local branch; never forces, stashes or discards |
| `GET /v2/git/receipts/{requestId}` | `gitReceipts` | This device's receipt for a request |

**Preconditions.** Status (and the branch list) now carries `preconditions`
{`branch`, `head`, `index`, `worktree`} and a `fingerprint` per entry. `index`
digests every index-side difference from HEAD (object ids and modes) plus any
operation in progress. `worktree` digests every listed record, together with
each unstaged, untracked or conflicted file's size, mtime, ctime, inode and
mode (an untracked folder covers its contents, up to 4,096 entries). The phone
sends them back unchanged. Under the worktree lock, the Mac resolves the folder
again, reads status again, and checks:

- every operation: branch and HEAD;
- stage/unstage of files: each file's fingerprint;
- stage all: `worktree`; unstage all and commit: `index`;
- create branch: that the start point (`HEAD` or a local branch) still names
  `startOid`; switch: that the target still names `oid`.

A mismatch is a `rejected` receipt with code `refresh_required`. Git still
decides everything else; its refusal is returned with its own output.

**Execution.** Mutations run Git with a fixed argument list, paths passed
NUL-separated on stdin (`--pathspec-from-file=- --pathspec-file-nul`) with
literal pathspecs, an explicit cwd and the noninteractive environment from
slice 1. Git runs in its own process group, and the whole group (hooks
included) is killed at the deadline: 60 s for index and branch operations,
120 s for commits. The work runs in a detached task, so a phone that
disconnects cannot cancel Git half-way. A commit is refused while a merge,
rebase, cherry-pick or revert is in progress (`operation_in_progress`; slice 3
adds Continue/Abort). Failed commits are classified as `nothing_to_commit`,
`identity_missing`, `signing_failed`, `hook_failed` (only when a commit hook is
installed) or `git_refused`. A refused switch returns `would_overwrite` with
the files Git listed. No route stashes, resets, cleans or forces.

**Receipts.** `requestId` is a UUID (case-insensitive) scoped to the device the
paired proxy proved. Receipts live in
`$LATCH_HOME/git/receipts/device-<id>/` (`local/` for loopback clients
without a device). A request is claimed by writing a complete, flushed
`pending` receipt before Git runs, and the Mac records HEAD, branch and target
paths just before starting Git. The same id with the same arguments returns
the first receipt (202 while it is still running) and never runs Git again.
The same id with other arguments is `409 request_id_conflict`. Another
device's id is invisible: `404 receipt_not_found`, exactly as for an unused
id.

A receipt left `pending` by a gateway that stopped is reconciled on the next
lookup or repeat, under the worktree lock:

- Git never started: `rejected` / `not_applied`.
- A commit whose branch did not move: `not_applied`.
- A commit that is one commit past the recorded HEAD with the requested
  subject: `completed`.
- A branch created at the start commit, or a branch that is now checked out:
  `completed`.
- A stage or unstage whose files all left (or reached) the staged area:
  `completed`.
- Anything else, or an `index.lock` still present: `uncertain`.

Interrupted stages and unstages are never reported as not applied. The same
reconciliation decides a timed-out mutation. Accepted IDs and receipts are retained per device; deleting settled receipts would make an old replay unsafe. Future compaction must retain durable deduplication tombstones.

**Phone.** Latch Mobile always resends a request whose answer was lost under
the same request ID, and only after its receipt has been read. A 404 receipt
means the Mac never accepted it.

**Limitations.** The recheck happens immediately before Git starts. An
external process that changes the worktree in the milliseconds between the
recheck and Git's own index lock is not detected. Commit hooks run against a private copy of the reviewed index; after pre-commit and message hooks the tree is checked. A hook that stages different content causes refresh required without a commit or a change to the real index. Its working-file edits remain for review. Untracked folders over 4,096 entries cannot be staged from the phone, and a truncated status cannot be committed. Receipts are retained per device, not
time-limited.

## Implemented: slice 3 (fetch, update, merge, rebase, Continue, Abort)

Every route is at **Control** in the shared route table and advertised by its
own key, so a phone offers only what the Mac serves. Mutations reuse the
slice-2 path: a device-scoped request id, a pending receipt written before Git
runs, the per-worktree lock, and a fresh recheck under that lock.

| Route | Key | Purpose |
| --- | --- | --- |
| `GET /v2/git/sync?path=&cursor=` | `gitSync` | Upstream (name, ref, remote, URL with credentials removed, commit as last fetched), remotes, ahead/behind, relation, why fast-forward is unavailable, incoming commits 50 per page. Never fetches; a cursor is refused with `409 stale_cursor` once the upstream moves |
| `POST /v2/git/fetch` | `gitFetch` | `git fetch --progress <remote>` for a remote `git remote` lists, with the Mac's own config and credentials |
| `POST /v2/git/fast-forward` | `gitFastForward` | `merge --ff-only` to the reviewed `upstreamOid`; never falls back to merge or rebase |
| `POST /v2/git/merge` | `gitMerge` | Merge the reviewed commit, with Git's default-style message |
| `POST /v2/git/rebase` | `gitRebase` | Rebase the current branch onto the reviewed commit |
| `POST /v2/git/continue` | `gitContinue` | `merge --continue` / `rebase --continue`, accepting Git's prepared message |
| `POST /v2/git/abort` | `gitAbort` | `merge --abort` / `rebase --abort`; refused without `confirmed: true` |
| (stage) | `gitStageResolved` | `POST /v2/git/stage` accepts conflicted files, marking them resolved |

**Opening never fetches.** Status, branches and the sync preview read only
local refs. Ahead/behind are against the tracking ref as last fetched, and
`lastFetchedAt` (FETCH_HEAD's time) changes only after an explicit fetch.

**Preconditions.** Fetch rechecks branch and HEAD and that the remote still
exists. Updates also recheck `index` and `worktree`, that no operation is in
progress, that the branch still tracks `upstream`, and that it still points
at `upstreamOid`. Continue and Abort recheck branch, HEAD, `index`,
`worktree` and that the named operation is the one in progress. Continue
refuses with `unresolved_conflicts` (naming the files) while any entry is
conflicted. A mismatch is `refresh_required`.

**Never stashes, resets or forces.** Merge and rebase pass `--no-autostash`,
so a Mac's `merge.autoStash` / `rebase.autoStash` cannot stash on the
phone's behalf. Git's refusals keep their output: `not_fast_forward`,
`would_overwrite` (with files), `local_changes`, `hook_failed`,
`git_refused`.

**Stops.** A merge or rebase that stops part-way is `completed` with code
`stopped` and the conflicted files, because Git did what was asked and is
waiting for the user. A rebase can also stop with nothing conflicted (an
`edit`, or a step that needs a terminal); Continue is offered and Git's own
refusal is returned if it refuses. Status reports the operation's refs:
`headName`, `onto`, `stoppedAt` (rebase), and `mergeHead`, `summary` (merge),
plus step/total. Conflict editing stays on the Mac. Staging a conflicted file
that still contains conflict markers is refused with `conflict_markers`
until the phone sends `confirmConflictMarkers: true` in a new request.

**Network and noninteractive execution.** Git runs as the leader of its own
session (no controlling terminal for `ssh` or a helper to prompt on), with
`GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS`/`GIT_EDITOR` set to `false`,
`SSH_ASKPASS_REQUIRE=never`, `GCM_INTERACTIVE=never` and
`credential.interactive=never`. Fetch failures are classified as
`auth_failed`, `network_failed`, `remote_not_found` or `git_refused`, with
Git's output.

**Progress, timeouts and Stop waiting.** These routes answer within about
20 s: settled (200), or the pending receipt (202) with `startedAt` and the
latest `progress` line (and percent) Git wrote. The phone keeps reading
`GET /v2/git/receipts/{id}`. Git is killed, with its whole process group, only
at its deadline: 300 s for fetch and rebase, 180 s for fast-forward, merge,
Continue and Abort. A timeout settles as `uncertain` / `timed_out`. Neither a
dropped connection nor Stop waiting stops Git, and the phone says so; it reads
the receipt again before offering anything new. There is no phone-initiated
cancel.

**Reconciliation after a restart.** Receipts record the source commit,
FETCH_HEAD's time and the operation in progress before Git starts.
- A fetch is `fetched` only if FETCH_HEAD was rewritten and is non-empty.
  Otherwise it is uncertain ("fetching again is safe"); a fetch killed at its
  deadline is always uncertain.
- An update whose branch did not move is `not_applied`. One that reached the
  reviewed commit (fast-forward), has the reviewed commit as second parent
  (merge) or contains it (rebase) is completed.
- A merge or rebase left in progress after an interruption, and anything
  else, is `uncertain`.
- Continue and Abort are judged against the recorded operation the same way.

**Phone.** The Sync sheet shows the remote card with Fetch, incoming commits,
Fast-forward only (the default, disabled with the reason on divergence),
Merge and Rebase, and `origin/main → main`. During a merge or rebase,
Continue and Abort… replace Commit and Sync, under an orange operation card.
Review push opens the slice-4 push review.

**Limitations.**
- The recheck happens immediately before Git starts, so a change in the
  following milliseconds is not detected.
- Progress lives in the gateway's memory and is absent after a restart.
- `fetch.writeFetchHead=false` makes an interrupted fetch always uncertain.
- Hooks (`pre-rebase`, `pre-merge-commit`, `commit-msg`) run on the Mac. A
  failure is labelled `hook_failed` only when such a hook is installed.
- Credential setup happens on the Mac.

## Implemented: slice 4 (push)

Every route is at **Control** in the shared route table, so the loopback
router and the paired proxy enforce it identically, and each is advertised
by its own key. A phone offers Review push only when the Mac advertises
`gitPushPreview`, `gitPush` and `gitReceipts`, and offers "Check the remote"
only with `gitPushReconcile`.

| Route | Key | Purpose |
| --- | --- | --- |
| `GET /v2/git/push/preview?path=&remote=&destination=&cursor=` | `gitPushPreview` | Source branch and commit, the remotes, the reviewed remote (name, push URL with credentials removed), destination branch, its remote-tracking ref and commit as last fetched, whether it is the remote's default branch or a new branch, ahead/behind, and the outgoing commits 50 per page. Never contacts the remote. A cursor is refused with `409 stale_cursor` once the source or destination moves |
| `POST /v2/git/push` | `gitPush` | Push exactly the reviewed commit to exactly the reviewed branch with a normal push |
| `POST /v2/git/push/reconcile` | `gitPushReconcile` | Only when the user asks: settle this device's uncertain push by asking the remote where the destination is |

**Choosing the destination.** Without `remote` and `destination` the
preview reviews the branch's upstream when that upstream is a branch on a
remote (`selectedBy: upstream`). Otherwise there is no target:
`selectionRequired` is true, with a reason and a `suggestion` (the branch's
own name on `origin`, or on the only remote) that the phone may pre-fill. Only
when the phone names both values, after the user confirms them, does a
target exist (`selectedBy: request`). A push never sets an upstream. When the
destination is the remote's default branch as last fetched
(`refs/remotes/<remote>/HEAD`), `isDefaultBranch` is set and the phone shows
a neutral note. The push itself is reviewed exactly as for any other branch.

**What is pushed.** `git push --porcelain --progress --no-follow-tags
--recurse-submodules=no --end-of-options <remote> <sourceOid>:refs/heads/<destination>`.
The exact reviewed commit is sent, and there is no force, lease, tag or
submodule. The Mac's own credentials, `pushurl`, hooks and signing settings
apply. Nothing from the phone reaches Git as an option or refspec: remote and
branch names are checked like slice 3's and against `git check-ref-format`,
and commits must be full hex ids.

**Preconditions.** These are rechecked under the worktree lock immediately
before Git runs:

- branch and HEAD;
- that the reviewed source branch is still checked out and still names
  `sourceOid`;
- that the remote still exists and its redacted push URL is the one
  reviewed;
- that the destination's remote-tracking ref still names `destinationOid`
  (or is still unknown).

Any change returns `refresh_required` and the user reviews the push again. The
index and working tree are not part of a push's preconditions, so an agent
editing files does not block it. A push is refused while a merge, rebase,
cherry-pick or revert is in progress (`operation_in_progress`).

**Outcomes.** These come from Git's own porcelain report for the destination
ref:

- `pushed`: a fast-forward or a new branch, with `outcome.commit` naming the
  pushed commit.
- `up_to_date`.
- `non_fast_forward`: the remote has commits this Mac lacks, including a
  remote that moved after review. The next step is to fetch and choose how
  to update. Latch never retries or forces.
- `remote_rejected`: a protected branch or a server hook, with the remote's
  reason.

Before any ref is reported:

- `auth_failed`, `network_failed` and `remote_not_found` come from Git's
  stderr.
- When a `pre-push` hook is installed and Git reports no destination result, `hook_failed` is uncertain; the output is preserved and the remote must be checked.
- Without a definitive destination report, other failures are `uncertain` / `push_unconfirmed`. A configured pre-push hook is insufficient evidence that the hook caused the failure; its no-report result is uncertain with the hook diagnostic.

Git's output, bounded to 16 KiB, is on every refusal. Credentials, `ssh`
host keys and helpers cannot prompt, because the slice-3 noninteractive
session applies. The deadline is 300 s, after which the whole process group
is killed. The submission answers within about 20 s with the pending receipt
and Git's latest progress line.

**Lost responses, restarts and Stop waiting.** Push requests are
deduplicated by device-scoped request id like every mutation, and the
pending receipt is written before Git runs. The Mac records Git's process
group for a push. A repository's tracking ref cannot prove the outcome of an interrupted push: a fetch or another Mac client can change it. Every started push left pending by restart, or killed at its deadline, remains uncertain until the user explicitly checks the remote. Only a receipt recorded before Git started can say `not_applied`. A missing process record is unknown, not proof that Git stopped. Process groups are recorded for all mutations; a still-running or unrecorded abandoned non-push operation also stays uncertain.

Stop waiting only dismisses progress. It never cancels Git. The phone reads the receipt again on return, retains its repository runner across back-navigation, and resolves the current adapter on each request after reconnect (bound to the original Mac identity). Repository data and these models are held in memory; recents are still Mac-owned.

**Checking the remote.** This happens only when the user asks. The Mac runs
`git ls-remote --refs --end-of-options <push-url> refs/heads/<destination>` with a 60 s deadline and the noninteractive environment, under the worktree lock. It queries the reviewed push URL, including a configured pushurl, and refuses to substitute a changed URL. A remote with multiple push URLs cannot be pushed from a single-target phone review.

The receipt completes only when the destination is at the pushed commit, or at a locally known descendant that contains it. An absent branch, unchanged destination or known commit without the source does not prove the earlier request never ran or that a remote receive process has stopped. These outcomes stay uncertain and offer no new push. This is deliberately conservative after a lost response.

Only `not_applied` lets the phone offer **Push again**, which is a new
review and a new request id. A receipt that cannot be read, or a check that
failed, never does. Checks are device-scoped (`404 receipt_not_found` for
another device), refused for other operations (`409 not_a_push`) and while
Git is still running here (`409 still_running`). They answer `409
worktree_busy` after waiting 20 s for another change in the same worktree.

**Limitations.**

- The recheck happens immediately before Git runs. The remote can still
  change between the review and the push; Git's own non-fast-forward refusal
  covers that.
- A destination's last-fetched commit is known only under the default fetch
  refspec (`refs/heads/*:refs/remotes/<remote>/*`). Under any other mapping it
  is reported as unknown and a push is reviewed as creating the branch.
- A server may finish after the local process stops. Negative remote observations therefore never enable a replacement push after a started, uncertain request. Settle it on the Mac if repeated checks cannot establish success.
- Process-group liveness uses the recorded id. A reused id can only keep a
  push uncertain, never settle it.
- Push-certificate signing (`push.gpgSign`) with an interactive pinentry is
  bounded by the deadline, not prevented.

## Remaining proposals

Not built, and not exposed anywhere:

- force push;
- reset and clean;
- branch deletion;
- partial (hunk) staging;
- conflict editing on the phone;
- setting an upstream on first push.

The [independent verification report](verification/git-management-2026-10-13/REPORT.md) records tested paths, screenshots, unexercised flows and release gates. This is not a blanket release approval.

## Verified review and response limits

Git responses have a final serialized JSON bound below 512 KiB, including escaping and session metadata. Oversized results return a small `response_too_large` error. For a mutation, the phone treats this and `receipt_unavailable` as an unknown outcome and reads the same receipt; neither permits a blind replacement action. Sync cursors bind both HEAD and upstream; log fields are NUL-delimited so unusual commit text cannot remove a commit from review. Failed status refreshes remain stale until a fresh response arrives.

At accessibility Dynamic Type sizes, Commit, Sync and Push action areas scroll with the review. At standard sizes they retain the normal bottom action bar. Commit does not force keyboard focus at accessibility sizes.
