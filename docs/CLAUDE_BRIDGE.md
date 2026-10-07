# The Claude bridge: Latch's function-hooks plugin

**Status:** implemented for Claude Code builds that load function hooks
("mods"). It is additive: a session without it behaves exactly as before.

## Why it exists

The Conversation Hub observes Claude from the outside. It tails the transcript
Claude writes, reads the terminal, and captures three command hooks
(`SessionStart`, `PermissionRequest`, `Stop`). Everything it does *to* the
session is a keystroke typed into the terminal.

That leaves real gaps, and each one is recorded as a blocker in
[the rich conversation view](FEATURE_RICH_CONVERSATION_UI.md):

- An interrupted turn fires no `Stop` hook, so the Hub cannot tell it ended.
- A question is not in the transcript while it is open. The only sign was
  Claude's permission hook, which carries no call id and reads
  "Allow AskUserQuestion?".
- An answer is a numbered key press, so only an offered label can be chosen.
  Free text and multi-select cannot be confirmed to land where they were aimed.
- A message is pasted into the composer, so sending needs an empty composer and
  a screen read to prove it.

A Claude Code mod is a TypeScript module the engine loads from a plugin. It
hooks the engine's own events and calls the engine's own operations. The bridge
is Latch's mod: the inside half of the conversation connector.

## What Latch installs

`latch` writes two private plugin directories under `$LATCH_HOME/observers/`
and passes each to Claude with `--plugin-dir`:

| Plugin | Kind | Role |
| --- | --- | --- |
| `latch-conversation-observer-v2` | command hooks | Source binding, permission requests, `Stop`. Unchanged. |
| `latch-conversation-bridge-v2` | function hooks | Everything below. |

They are separate on purpose. A Claude build that cannot load a hooks module
skips the bridge and still runs the observer. Set `LATCH_CLAUDE_BRIDGE=0` in
the environment of the process that creates the session to launch with the
observer alone.

The module's source is `crates/latch/assets/claude-bridge/hooks/register.ts`,
compiled into the `latch` binary. `hooks/config.ts` is rewritten at launch with
the absolute path of that binary. Like the observer, the directory name carries
a version, so a running Claude keeps the module it loaded.

## How the two halves talk

A hooks module has no file or socket API of its own. Every exchange is one run
of the `latch` binary that launched the session, which inherits
`LATCH_SESSION_ID`:

| Command | Direction | Effect |
| --- | --- | --- |
| `latch __conversation-bridge hello` | module to Hub | Appends the greeting to the hook sidecar, creates the inbox, answers `{ "inbox": path }`. |
| `latch __conversation-bridge event` | module to Hub | Appends one record to the hook sidecar. |
| `latch __conversation-bridge take` | Hub to module | Hands over every queued command, removing each as it does. |

Records land in the session's existing `conversation-source-hooks.jsonl`, each
with `hook_event_name: "LatchBridge"` and a `bridge_event`. A `latch` older
than the bridge ignores a hook name it does not know.

Commands are private files in `<session>/conversation-bridge-inbox/`. The
module looks at that directory twice a second and runs `take` only when
something is waiting. Removal is the hand-over: a command the connector can
still remove was never seen by the agent, so falling back to the terminal is
safe. A command that was taken but never answered is reported as ambiguous and
is not retried.

### Records the module writes

| `bridge_event` | Fields | Used for |
| --- | --- | --- |
| `hello` | `bridge_version`, `claude_version`, `model`, `capabilities`, `commands[]` | Marks the bridge live. `commands` is the session's slash-command catalog. |
| `turn.start` | `turn_id` | Opens the turn. |
| `turn.complete` | `turn_id`, `reason` (`answer`, `aborted`, `refusal`, `error`), `duration_ms` | Closes the turn, including an interrupted one. Main loop only. |
| `question.open` | `tool_use_id`, `questions[]` with options, descriptions, `multi_select` | Announces a question under its real call id. |
| `question.closed` | `tool_use_id`, `answered_by` (`latch`, `terminal`, `nobody`) | Dismisses it. |
| `permission.open` | `tool_use_id`, `tool`, `input` (the call's named scalar arguments, each bounded to 1000 characters) | Announces an open permission dialog under the call's real id. |
| `permission.closed` | `tool_use_id`, `answered_by` (`latch`, `terminal`, `nobody`) | Dismisses it. |
| `command.result` | `command_id`, `outcome` (`accepted`, `queued`, `refused`), `detail` | Answers one command. |
| `session.end` | `reason` | Retires the bridge, except on `/clear`, which keeps the process. |

Every record carries a millisecond `timestamp`. The Hub reads the sidecar
before the transcript, so on a late open it sees every turn close before the
prompts that preceded them. The timestamp is what stops an old prompt from
reopening a closed turn.

### Commands the module carries out

| `kind` | Payload | Engine call |
| --- | --- | --- |
| `submit_prompt` | `text` | `$.prompt.submit({ text, asUser: true })`. The model reads it as the person's own words. Entered while a turn runs, it is reported `queued`. |
| `abort_turn` | none | `$.turn.abort` on the running turn. |
| `answer_question` | `tool_use_id`, `answers` (question text to answer) | Settles the open `AskUserQuestion` call with that id. |
| `answer_permission` | `tool_use_id`, `decision` (`allow` or `deny`) | Settles the open permission dialog for the call with that id. |

The question and permission hooks race the two places an answer can come from.
The engine's dialog still opens at the terminal. Whichever answers first
settles the call, and an answer from Latch abandons the dialog beneath, so
both can never answer.

A permission is the engine's to grant, and a hook cannot settle an open
dialog as allowed. The module hooks every `tool.call` of the model's. When
the engine raises its `PermissionRequest` hook for the call (which it does
only as a dialog opens, so a call a rule or the mode settles is never
announced), the module matches the dialog to the running call by tool and
arguments and reports `permission.open` under the call's id. A `deny` from
Latch is returned with the dialog open. An `allow` abandons the engine's path
for the call and runs the same call again through `$.tool.call` with a
`consent` line; the module's own `tool.check` hook approves that one re-run
and nothing else, because the engine's `next.origin` names this plugin for
it and the engine for the model's own calls. The re-run's result is returned
as the original call's, so the transcript shows one call with one result.

## What the Hub does with it today

- **Turn state.** `turn.start` and `turn.complete` are authoritative. An
  interrupted turn returns to idle without waiting on the screen.
- **Turn outcome.** The `reason` on `turn.complete` reaches clients as
  `state.turnOutcome`: `answer`, `aborted`, `refusal`, or `error`. It is
  carried only while no turn is open, is cleared the moment any turn opens
  (bridged or not), and a reason this Latch does not know is omitted rather
  than guessed. The mobile status line reads "Stopped", "Agent declined the
  last request", or "Last turn failed" between turns; an ordinary answer shows
  nothing. The outcome survives a Hub checkpoint.
- **Command catalog.** The greeting's `commands` reach clients as
  `state.commands`, each `{ name, description, source }`, bounded to 200
  entries with names of at most 128 characters, descriptions of 512, and
  sources of 64. It is present only while the bridge is live: a greeting
  without a catalog, a session without a bridge, or a bridge that has ended
  leaves the key absent, which clients read as unknown, never as none. `/clear`
  keeps the loaded module and so keeps the catalog. The mobile store exposes
  it as `commands`. In LatchMobile, a draft beginning with `/` offers matching
  command names with their descriptions and sources. Picking one inserts
  `/name ` into the draft without sending it; typing arguments hides the list.
  An absent or empty catalog offers nothing. The list is disabled while the
  agent works, and both selection and sending a slash draft require a connected,
  send-enabled idle turn. Ordinary bridge messages can still be queued mid-turn.
- **Sending.** With a live bridge, `send_message` goes through `submit_prompt`
  and no longer needs an empty composer. A text beginning with `/` still goes
  to the terminal, because the engine runs a slash command only from its own
  prompt box.
- **Stopping.** `cancel_turn` calls `abort_turn` only with a live bridge and an
  open turn. It requires the interact grant and uses the same deduplicated
  operation receipts as sending. The acknowledgement does not close the turn;
  `turn.complete` does.
- **Questions.** The pending request is the real question, with its call id.
  The v2 request carries optional `questions` with each question's exact text,
  header, options with descriptions, and `multiSelect`. Mobile shows one form
  per question, with free text and multiple selections where offered. It submits
  `resolve_request.answers` as a question-text-to-answer map: exactly one nonempty
  answer per question, with selected labels joined by commas. Single-selection
  questions use either an offered label or free text. Each submission targets
  the request's real call id and uses the existing operation receipts; accepted
  or in-flight answers cannot be submitted again.
  Older clients can still send `choice` for a single question or a permission.
  A multi-question request requires `answers`; incomplete, extra, and stale
  answers are refused. Structured answers require a live bridge and never
  fall back to terminal keys. An untaken answer disables bridge answering;
  a taken command with no result remains ambiguous and is never resent.
- **Permissions.** With a live bridge the pending request is the call itself:
  `permission.open` announces it under its real call id, with the same prompt
  the permission hook would give it (the input's `description`, else
  "Allow <tool>?") and the choices `Yes` and `No`. The engine's permission
  hook for the same dialog is ignored while that request is pending, and a
  hook announced first is replaced by the call id. Transcript records do not
  dismiss it; `permission.closed` does, as does the dialog leaving the
  screen. Once the dialog is painted the labels Claude shows replace the
  seeded choices. `resolve_request` with `Yes` or `No` (`Allow` and `Deny`
  are read the same way) goes through `answer_permission` by call id, under
  the interact grant and the same deduplicated operation receipts as every
  other action. Any other label, such as "Yes, and don't ask again", writes a
  rule only the dialog can write, so it is never handed to the bridge and
  takes the screen-verified key path as before. A permission the hook
  announced without a bridge call id also stays on the key path.
- **Several dialogs at once.** A parallel batch can raise several dialogs
  together (see [When the engine raises the permission hook](#when-the-engine-raises-the-permission-hook)).
  The Hub keeps every request the bridge announces, by call id, and shows one
  at a time. The others wait behind it in the order the engine raised them,
  by the bridge's millisecond timestamps rather than by arrival, because each
  record is its own run of `latch` and they can reach the sidecar out of
  order. Clients see nothing of a waiting request until it is shown. The
  shown request is the oldest, unless the screen proves Claude is painting a
  waiting one: then that one is shown, and the one it displaced stays
  pending at the Hub until it is shown again or closes. When the shown
  request closes, by `permission.closed`, by `question.closed`, or by an
  answer from Latch, the request displaced most recently is shown again,
  else the oldest still waiting. A waiting request that closes first, for
  instance one answered at the terminal, is dropped by its id; one clients
  never saw leaves no trace. The classic hook's own records for the batch
  are ignored while a bridge request is shown. Only the bridge says when a
  waiting request closes, so when the bridge ends they are forgotten and
  left to the terminal. The queue survives a Hub checkpoint.
- **Keys with several dialogs.** The key path presses a number into whatever
  dialog is on the screen, so with other requests waiting it requires the
  screen to prove the shown request's dialog is the one painted: its prompt
  is the last on the screen with numbered decisions beneath it, and no other
  waiting request shares that prompt. Otherwise the label is refused with a
  reason, and `Yes` and `No` still go through the bridge by call id. Only the
  painted request's labels replace the seeded `Yes` and `No`.

Sending, legacy single-choice answers, and permission answers fall back to
the terminal path when the module does not take the command, and the
connector then treats the bridge as gone. Sending through the terminal still
requires an idle turn and an empty composer; a permission answer still
requires the dialog on the screen and the chosen label among its visible
numbered decisions. `cancel_turn` never falls back to the terminal: an
untaken command disables the bridge and is refused. Commands taken without a
result remain ambiguous and are never retried automatically.

## Planned features and the hook that serves each

| Planned feature | Hook or call | State |
| --- | --- | --- |
| Completed, interrupted, and failed turn states | `turn.complete` `reason` | Implemented: `state.turnOutcome` carries the reason between turns, and mobile labels an interrupted, refused, or failed turn. |
| Stop button | `abort_turn` | Implemented as `cancel_turn`, advertised by `state.cancelTurn` only while the bridge is live and a turn is open. Mobile Stop cancels the turn without ending the session. |
| Free-text answers | `answer_question` | Implemented in the Hub and mobile structured question form. |
| Multi-select answers | `answer_question` | Implemented: `multiSelect` reaches clients, and mobile submits comma-joined selected labels plus optional free text. |
| Several questions in one call | `answer_question` takes a map | Implemented: structured questions reach clients and `resolve_request.answers` supplies one answer per question. |
| Option descriptions on question cards | `question.open` | Implemented in the contract and mobile form. |
| Advertised agent commands | `hello.commands` | In the contract as `state.commands` while the bridge is live. LatchMobile offers filtered slash completions with descriptions and sources; selection inserts the name into the draft and requires an idle turn. |
| Send while the agent works | `submit_prompt` reports `queued` | Implemented for a live bridge. Queued sends carry `queued` in their operation receipt and message status until transcript observation reconciles them. Slash commands still require an idle terminal. |
| Permission answers by request id | `classic.PermissionRequest`, `tool.call`, `tool.check` | Implemented: `permission.open` carries the call id, and `answer_permission` settles the dialog. Rule-writing labels stay on the key path. The live run that cleared it is recorded under [the permission probe](#the-permission-probe). |
| Several permission dialogs from one batch | `permission.open`, `permission.closed` per call id | Implemented in the Hub: an ordered queue by call id, one shown at a time, the next shown as each closes. No contract change: clients see one pending request, as before. |
| Streaming assistant text | `turn.step` | Not built. See below. |

## Known limits

- **Permissions.** An allow from Latch is a second run of the call under a
  new id, not the dialog's own "Yes": the engine records the re-run as the
  original call's result, and the person reads one call in the transcript,
  but the engine's own log of the abandoned path says the call was
  rejected. Only `allow` and `deny` are carried; a label that also writes a
  permission rule is the dialog's and stays on the key path. Several dialogs
  from one batch are shown one at a time; clients are not told how many
  wait behind the shown one, and a rule-writing label is refused while the
  screen cannot tell the waiting dialogs apart. A waiting request the screen
  painted after clients were already shown it cannot be brought back in
  front of a newer one, because the Hub presents the newest pending request;
  its key path is refused until it is the shown one again. A
  dialog raised in a subagent's loop is announced like the main loop's. In
  auto mode the classifier settles an `ask` before any dialog, so nothing is
  announced unless it escalates. The engine's `PermissionRequest` hook
  carries no call id, so the dialog is matched to the running call by tool
  and arguments, with the only waiting call of that tool as the fallback.
- **Streaming.** A `turn.step` hook sits on every model request of the session.
  A fault there costs more than the feature is worth until the rich contract
  has somewhere to put a streaming row.
- **`@file` mentions and pasted images** are not expanded in a prompt a plugin
  submits. Attachments by workspace path are unaffected.
- **The mod API is early access** and may change between Claude releases. The
  module's tests run against the installed engine:

  ```bash
  claude plugin validate crates/latch/assets/claude-bridge
  claude plugin test crates/latch/assets/claude-bridge
  ```

- **Existing sessions** keep the plugins they were launched with. Only sessions
  created by a `latch` that has the bridge gain it.

## When the engine raises the permission hook

Read from the installed engine, Claude Code 2.1.292, on 7 October 2026.
The engine's type kit draws no dialog, so it cannot show this, and no
Latch session had yet recorded a permission.

- The tool executor runs consecutive calls whose tool is concurrency-safe
  for that input together, and every other call alone, once the calls
  running before it have finished. A call's permission check is part of
  running it.
- A call whose check reaches the ask path builds its dialog request and, in
  the same step, starts the `PermissionRequest` hooks, before the dialog is
  queued for the screen. The hook does not wait for the dialog to be
  painted.

So a batch of concurrency-safe calls that each need the person (reads
outside the workspace, `WebFetch`, read-only `Bash`, read-only MCP tools)
raises the hook for every call at once, and the bridge announces each, while
only the first dialog is on the screen. A batch of calls that are not
concurrency-safe (`Bash` that writes, `Edit`, `Write`) raises one dialog at a
time: the next call's check runs only after the previous call settled, which
for a call waiting on its dialog means after that dialog was answered. Both
are covered by the queue above. The bridge's own test,
`a parallel batch announces every dialog under its own call id`, raises
three dialogs together and answers them from Latch and the terminal in mixed
order. Watching it live needs a batch of concurrency-safe calls that each
need the person, for instance three `WebFetch` calls to different hosts that
no rule allows.

## The permission probe

`docs/experiments/claude-permission-probe` is a standalone hooks module that
answers the question the table above leaves open: can a module answer a
permission prompt by call id? It is not loaded by Latch. It simulates Latch's
answer with a timer so it can be run with nothing but Claude.

The pair of hooks it uses:

- `tool.check` on `Bash` passes the engine's verdict through and, when a
  one-shot approval is held for the command, answers `allow` instead. The
  engine's `next.origin` names the plugin for a call the module itself makes,
  so a production module would approve only its own re-run of a call Latch
  allowed, never the model's next identical call.
- `tool.call` on `Bash` lets any ordinary command through. For a command that
  contains `LATCH_PERM_PROBE_ALLOW` or `LATCH_PERM_PROBE_DENY` it starts the
  engine's own path (`next(e)`, which is the permission check, the dialog, and
  the tool) and races it against the simulated answer, six seconds later:
  - the terminal answering first is returned as is;
  - a deny returns `{ deny }` while the dialog is open, as the question hook
    already does for a question;
  - an allow records the one-shot approval and calls `$.tool.call` with the
    same command and a `consent` line, then returns that run's result as the
    original call's. The abandoned engine path is logged when it settles.

What the engine's test kit proves (`claude plugin test docs/experiments/claude-permission-probe`):
the allow path runs the tool once more under a new call id and returns its
result as the original call's; the one-shot approval is read by `tool.check`
exactly once; the deny path ends the call while the engine's path is still
open. The kit reaches the test's own `tool.call` without raising `tool.check`
and draws no dialog, so two things needed a live session: that the engine's
re-run reads the module's approving verdict rather than asking again, and
that the abandoned dialog leaves the screen.

**The live run (Claude Code 2.1.292, 7 October 2026).** A person ran the
sequence below in an interactive session and left the first two prompts
unanswered. The log recorded, for the allow path, the engine's `ask` verdict
for the model's call, the simulated allow six seconds later, the re-run's
`tool.check` arriving with `origin` naming the probe plugin and answered
`allow` without a dialog, the re-run settling with the tool's result, and
the abandoned engine path settling three milliseconds later with the
engine's own rejection ("The user doesn't want to proceed"). The file was
created and the transcript shows one Bash call with one result. For the deny
path the call ended with the probe's denial, the engine path settled the
same way three milliseconds later, and no file was created. On the third
prompt the person pressed `1` within six seconds and the terminal won the
race. The person went on typing prompts after each unanswered dialog, so the
dialog had left the screen. That is what cleared the production bridge to
carry both halves.

To watch it, in an interactive session with nothing of Latch's inherited:

```bash
env -i HOME="$HOME" PATH="$PATH" TERM="$TERM" LATCH_PROBE_LOG=/tmp/latch-perm-probe.log \
  claude --plugin-dir docs/experiments/claude-permission-probe --permission-mode default
```

Then ask for `touch /tmp/probe-allow LATCH_PERM_PROBE_ALLOW` and leave the
permission prompt unanswered. After six seconds the file should exist, the
transcript should show one Bash call with a result, and the log should show the
re-run settling and the abandoned path settling after it. Ask for
`touch /tmp/probe-deny LATCH_PERM_PROBE_DENY` the same way: the prompt should
close with a denial and no file. Answer a third prompt at the terminal within
six seconds to see the terminal win the race. Whichever way it goes, the log at
`LATCH_PROBE_LOG` holds the sequence.

The production bridge differs from the probe in two ways. It announces the
dialog from the engine's `classic.PermissionRequest` hook rather than from
`tool.check`'s `ask` verdict, because that hook fires only as a dialog
opens, while an `ask` verdict in auto mode goes to the classifier first. And
its `tool.check` approves a re-run only when `next.origin` names the bridge
plugin, so the model's own next identical call is never approved by it. The
engine's test kit raises every check with the engine as origin, so the
module's tests prove the negative (an engine-origin check for the same
command is not approved) and the live run above proves the positive.

## Trust

The bridge adds no authority. A prompt it submits has the standing a typed one
has, and the Hub checks the same interact grant before queuing either. The
inbox is owner-only, and `take` is reachable only by a process that already
runs as the session's user with its `LATCH_SESSION_ID`.
