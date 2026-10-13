# Design: Git management on Latch Mobile

**Status:** implemented across slices 1–4: Repository, Diff, staging, Commit, Branches, Sync (fetch, fast-forward, merge, rebase), the merge/rebase operation card with Continue/Abort, Review push and the four outcome states, including checking an uncertain push against the remote. Independent verification and hardening ran on 2026-10-13; [the report](verification/git-management-2026-10-13/REPORT.md) separates passing evidence from remaining release gates. Force push, reset, clean, branch deletion, partial staging and conflict editing remain out of scope. Interface design for
[FEATURE_GIT_MANAGEMENT.md](FEATURE_GIT_MANAGEMENT.md).
**Mockups:** https://claude.ai/artifact/Bu2qc9HTnoBdkcxJSjxEo7 (11 phone screens).

The repository view is built only from parts the app already uses. Anyone who
knows the session list and the folder browser should recognize every control.

## Design language this follows

| Existing pattern | Where it lives today | Use in the repository view |
| --- | --- | --- |
| Floating chrome: back circle, title/status capsule, trailing "⋯" | `SessionChromeBar` | Repository screen and diff screen |
| `FloatingControl`: Liquid Glass on iOS 26, material before it; one tinted primary | "+ Session" pill and gear | **Commit** (tinted) and **Sync** (glass) at the bottom |
| Plain list with hidden separators and 12pt rounded row fills | Session list | Change rows |
| Outlined pill with a colored dot | `LinkStatusPill` | Stale/reconnecting state |
| Sheet with inline title, Cancel/Done, `.bar` footer, `.borderedProminent` button and a caption2 consequence line | `FolderPickerView` action bar | Commit, Sync, Push review |
| Yellow/orange tinted card for something waiting on you | `ConversationRequestCard` | Merge/rebase in progress, Git refusals |
| A control that stays visible and dims when the grant is missing, with an alert explaining why | New session, Stop | Every Git mutation |
| Face ID before any Mac file name is shown | Folder browser | Opening any repository |
| Swipe actions and a context menu on the same row | Session rows | Stage/unstage, copy path, open diff |

Accent stays `#255BF4` / `#3D8CFF`. Status letters use SF Mono in small
rounded chips (M blue, A green, D orange-red, R purple, ? gray, ! solid
orange-red for conflicts). Each status also has a letter and a VoiceOver
label, so color is never the only signal.

## Navigation

- **From the session list:** a third floating circle with an
  `arrow.triangle.branch` glyph sits beside the Settings gear. It opens the
  **Repositories** sheet, which has three parts: repositories from open
  sessions (each session's folder resolved to its worktree, with how many
  sessions are working there), **Recent on this Mac**, and **Browse folders on
  your Mac…**. The Mac keeps the recent list and reports it, so every phone
  paired with that Mac sees the same repositories. That last item reuses
  `FolderPickerView` with a new `.chooseRepository` mode, whose button reads
  "Open repository here".
- **From a session:** the session details sheet gets a "Repository · main ↑2"
  row, and the terminal/chat "⋯" menu gets "Open repository". Both open the
  repository for that session's working directory and **never attach** to the
  terminal.
- **On iPad:** the repository opens in the detail column, like a session.
- **Older Macs:** if the gateway doesn't advertise the Git capabilities, none of
  these entry points appear.

The repository is a pushed screen, not a sheet, because people stay on it
while they read diffs. Commit, Branches, Sync and Push are sheets because each
is one decision that ends with one button.

## Screens

### Repository

- **Chrome:** the capsule shows the repository name and `branch · ↑ahead ↓behind`.
  Tapping it opens **Branches**. The "⋯" menu holds Refresh, Fetch, Copy path,
  and Open session here, which goes to the new-session flow in this folder.
- **Header card:** the canonical worktree path (monospaced and head-truncated,
  as in the folder browser), the branch, "↑2 to push" and "↓1 incoming" pills,
  and "Tracks origin/main · fetched 4 min ago" beside a **Fetch** button. A
  detached HEAD shows the short SHA in place of the branch.
- **No automatic fetch.** The screen shows exactly the Git state on the Mac:
  opening or refreshing a repository never touches the network. Ahead/behind
  counts are against the remote-tracking ref as last fetched, and the header
  always says when that was. Fetch is the only way to update it from the phone.
- **Sessions working here:** when any Latch session's folder resolves to this
  worktree, an orange card under the header lists them as chips, for example
  "Claude · git-ui · editing" and "Terminal · zsh · idle". The dot is green
  while the session is producing output or an agent turn is running, gray when
  idle. Tapping a chip opens that session. The card says files may change while
  you look. It is information, not a lock: the stale-precondition check is
  still what stops a mutation from acting on a state the user didn't see.
- **Sections:** Conflicted, Staged, Changes, and Untracked, each with a count.
  A section with nothing in it is not shown. Section headers offer
  "Stage all"/"Unstage all". Each row shows the file name, the directory (or
  "was <old name>" for a rename) and `+n −m`, with a 44pt staging circle on
  the trailing edge. Tapping the row opens the diff; tapping the circle stages
  or unstages the file. Swipe and long-press do the same, as on session rows.
- **Bottom:** **Commit n** (tinted, enabled once something is staged) and
  **Sync ↓1 ↑2** (glass). During a merge or rebase both are replaced by
  **Continue** and **Abort…**.
- **Results** appear as a short glass toast above the bottom controls, for
  example "Fetched origin · 1 new commit on origin/main". Every result is
  followed by a status refresh.

### Diff

Opens as a pushed screen with the same chrome. The capsule shows the file name
and "Unstaged · +12 −4". Hunks are shown as unified diff text in monospace,
with line numbers and a +/− column, so additions and deletions don't depend on
color alone.

The view respects the gateway's limits. When a diff is cut short, a "Showing 2
of 3 hunks" card with **Load more** says so. Binary files, files over the limit
and submodules each get a `MessageView` explaining why there is no text diff.
At the bottom are **Stage file** / **Unstage file** and buttons to go to the
previous or next changed file. A conflicted file shows its markers read-only.

### Commit (sheet)

The sheet has a summary field with a 50-character guide and an optional
description. It lists the staged files with their line counts and says how
many unstaged files will be left out.

The footer button reads "Commit 2 files to main". Its caption is: "Creates a
commit on your Mac. Nothing is pushed." A note under the message explains that
the commit uses the Mac's Git identity and that commit hooks run on the Mac.

If a session in this worktree is active, an orange note above the button names
it and says Latch will ask for a review if the index changes before the commit
runs. Sync, Branches and Push review show the same note.

### Branches (sheet)

The sheet lists the current branch first, then search, **New branch from
main…**, and the local branches with their upstream and ahead/behind counts.

When Git refuses a switch, its refusal appears in an orange card at the top.
The card names the files Git listed and offers **Commit changes…** or **Show
Git's message**. Latch doesn't stash, discard, or offer to do either.

### Sync (sheet)

The sheet opens with the remote card: name, URL, when it was last fetched,
and **Fetch**. Below it are the incoming commits and three ways to bring them
in:

- **Fast-forward only:** the default. It is shown disabled, with the reason,
  when the branches have diverged.
- **Merge.**
- **Rebase.**

Above the button, the refs appear as `origin/main → main`. Its caption says
that if Git stops on conflicts, the user resolves them on the Mac and continues
on the phone. Outgoing commits lead to **Review push…**.

### Review push (pushed inside the Sync sheet)

The screen shows `main (this Mac · sha) → origin/main (remote · sha)`, the
remote name and URL, and every commit to send. If the destination is the
remote's default branch, a neutral info card says so, and the push goes
through the same review as any other branch. The button reads "Push 2 commits
to origin/main". Its caption is: "Uses the Git credentials on your Mac. Force
push isn't available from the phone."

**As built (slice 4):**

- **How it opens.** Review push opens from the Sync sheet, the repository
  header's "↑N to push" pill, or the ⋯ menu. It appears only when the Mac
  advertises `gitPushPreview`, `gitPush` and `gitReceipts`.
- **No upstream.** For a branch with no upstream on a remote, the Sync sheet
  shows a "No upstream yet" row. The review then asks the user to pick a
  remote and type a destination branch. The Mac's suggestion (usually
  `origin/<branch>`) is only pre-filled, and nothing is pushed until the user
  confirms it and the review reloads for that exact destination. Latch never
  sets an upstream.
- **What the review shows.** The header gives `main (this Mac · sha) →
  origin/main (remote · sha as last fetched)`, or "new branch". It also shows:
  - the remote's name and push URL, with credentials removed;
  - the default-branch info card;
  - an orange card when the remote, as last fetched, has commits the branch
    lacks ("the remote will probably refuse");
  - the outgoing commits, 50 at a time, with **Load more**;
  - the active-session note.
- **The request.** It is built only from the reviewed preview.
- **While Git runs.** Live progress shows Git's latest line. **Stop waiting**
  says Git may still be pushing on the Mac.
- **Outcomes:**
  - Completed is a toast followed by a refresh.
  - `non_fast_forward` leads to "Fetch and choose how to update".
  - `refresh_required` leads to **Review again**: the review reloads, and the
    push needs a new confirmation and a new request id.
  - `auth_failed`, `hook_failed` and `remote_rejected` show Git's output and
    what to fix on the Mac.
- **Uncertain pushes.** An uncertain push (timed out, or its answer lost)
  shows the timeline and **Check the remote** (only with `gitPushReconcile`),
  which stays disabled while the Mac asks. The check can end three ways:
  - "Pushed" or "The remote already had these commits";
  - a pre-start receipt saying Git never ran, the only state that offers **Push again**;
  - still uncertain, with the Mac's explanation.

  No new push can start while an earlier push is unconfirmed, even after the
  sheet is closed.

### Merge or rebase in progress

The repository screen shows an orange operation card. It names the operation,
the refs involved, and the commit where the rebase stopped (for example, 3 of
5). It tells the user to resolve the conflicted files on the Mac or in a
terminal session and then stage them here. **Continue** stays dimmed until no
conflicted files remain unstaged, and its caption says why.

**Abort…** uses the same `confirmationDialog` pattern as Stop. Staging a file
that still contains conflict markers asks first.

### Outcomes

Every mutation ends in one of four states. Each one is spelled out
differently:

| Outcome | Presentation |
| --- | --- |
| Completed | Toast plus a refresh. |
| Rejected by Git | Sheet result: red ✕, a plain-language headline, Git's own output in monospace, and the next sensible step as the primary button. Example: "Fetch and choose how to update". |
| Refresh required (stale preconditions) | The same rejected layout: "Repository changed on your Mac". The primary button refreshes. |
| Uncertain (lost response) | Orange "?". A timeline of what Latch knows: request accepted, receipt id, connection lost, last progress. "Checking the receipt on your Mac…" runs while the button stays disabled. It can resolve to "Pushed", "Git rejected the push", or a pre-start receipt proving Git never ran. Only that evidence offers **Push again**. An unchanged or absent remote destination after a started request remains uncertain. |

Long network operations show Git's progress in the sheet. **Stop waiting**
dismisses the progress view. It does not claim Git stopped, so the receipt
check runs on return.

### Stale and offline

When the link drops, the last known state stays readable, dimmed, under an
outlined pill: "Reconnecting · showing what Latch saw 4 min ago". This is the
same rule the session list follows. Diffs that are already open stay readable.
Every mutation waits for the link and a fresh status.

## Permissions and gating

- **Opening a repository** requires the device-owner check (Face ID, Touch ID
  or passcode), the same one the folder browser uses.
- **Without the Control grant**, Commit, Sync, staging circles and branch
  actions stay visible but dimmed. Tapping one raises "This phone can't change
  this repository" and says what to change on the Mac, as New session does.
- **Out of scope for v1, and never shown:** force push, reset, clean, branch
  deletion, partial-hunk staging, and conflict editing.

## Accessibility

- Every icon-only control has a label: Back, More, Previous/Next changed
  file, and the staging circle ("Stage FolderPickerView.swift").
- File rows combine into one element whose value is the status, for example
  "Modified, unstaged". The hint describes what a tap does.
- Ahead/behind is spoken as "2 commits to push, 1 incoming".
- Floating controls follow the existing `FloatingControlMetrics` Dynamic Type
  cap. Rows and sheets scale fully.

## Decisions

Settled on 2026-10-13:

1. **Recent repositories come from the Mac.** The Mac records repositories
   opened from any phone and reports them with the repository list, so every
   paired phone sees the same "Recent on this Mac" section. The phone stores
   nothing about them.
2. **No automatic fetch.** The view reflects exactly the Git state on the Mac.
   The header card has a visible **Fetch** button (also in the "⋯" menu and
   the Sync sheet), and the header shows when the last fetch happened.
3. **Show sessions in the same worktree.** The Repositories sheet shows a
   count per repository. The repository screen lists the sessions as chips
   with their activity, and the Commit, Sync, Branches and Push sheets warn
   when one of them is active. This needs the gateway to report, for each
   repository, which session IDs resolve to its worktree.

## Independent verification changes (2026-10-13)

- At accessibility text sizes, Commit, Sync and Review push place their action areas in the scrolling content, so a large warning cannot cover the review. Full action labels wrap. Commit leaves keyboard focus to the user at these sizes.
- Cached status stays dimmed and mutations stay blocked after a failed reconnect refresh.
- Repository models and pending IDs survive closing and reopening the screen within the app lifetime. Each request uses the replacement gateway after reconnect and remains bound to the same Mac. This does not yet persist pending actions across app termination.
- A started push is never reported completed from a local tracking ref, or not run from a negative remote observation. An uncertain push remains protected even on a Mac without reconciliation.
- Commit hooks cannot add files to the reviewed index; Latch refuses the commit and preserves the real index and hook edits for a new review.

See [the requirement matrix and screenshots](verification/git-management-2026-10-13/REPORT.md) for the actual validation scope, including physical owner-authentication and VoiceOver limits.
