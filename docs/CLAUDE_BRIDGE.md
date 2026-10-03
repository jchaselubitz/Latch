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
| `latch-conversation-bridge-v1` | function hooks | Everything below. |

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

The question hook races the two places an answer can come from. The engine's
dialog still opens at the terminal. Whichever answers first settles the call,
and an answer from Latch abandons the dialog beneath, so both can never answer.

## What the Hub does with it today

- **Turn state.** `turn.start` and `turn.complete` are authoritative. An
  interrupted turn returns to idle without waiting on the screen.
- **Sending.** With a live bridge, `send_message` goes through `submit_prompt`
  and no longer needs an empty composer. A text beginning with `/` still goes
  to the terminal, because the engine runs a slash command only from its own
  prompt box.
- **Stopping.** `cancel_turn` calls `abort_turn` only with a live bridge and an
  open turn. It requires the interact grant and uses the same deduplicated
  operation receipts as sending. The acknowledgement does not close the turn;
  `turn.complete` does.
- **Questions.** The pending request is the real question, with its call id.
  `resolve_request` for a single question goes through `answer_question`, and
  the choice may be any text: an offered label, free text, or comma-joined
  labels for a multi-select question.
- **Permissions** are unchanged. They are still announced by the permission
  hook and answered by the visible numbered key.

Sending and question answers fall back to the terminal path when the module does
not take the command, and the connector then treats the bridge as gone. Sending
through the terminal still requires an idle turn and an empty composer.
`cancel_turn` never falls back to the terminal: an untaken command disables the
bridge and is refused. Commands taken without a result remain ambiguous and
are never retried automatically.

## Planned features and the hook that serves each

| Planned feature | Hook or call | State |
| --- | --- | --- |
| Completed, interrupted, and failed turn states | `turn.complete` `reason` | Recorded. The Hub uses it to close the turn. Carrying the reason to clients needs a contract field. |
| Stop button | `abort_turn` | Implemented as `cancel_turn`, advertised by `state.cancelTurn` only while the bridge is live and a turn is open. Mobile Stop cancels the turn without ending the session. |
| Free-text answers | `answer_question` | Hub accepts them. The mobile question card needs a text field. |
| Multi-select answers | `answer_question` | Hub accepts comma-joined labels. The card needs multi-select, and `multi_select` must reach the client. |
| Several questions in one call | `answer_question` takes a map | Module done. The Hub still flattens the prompt, so these stay on the terminal until the contract carries structured questions. |
| Option descriptions on question cards | `question.open` | Recorded. Needs the rich contract. |
| Advertised agent commands | `hello.commands` | Recorded. Needs a capability in the contract and a composer surface. |
| Send while the agent works | `submit_prompt` reports `queued` | Implemented for a live bridge. Queued sends carry `queued` in their operation receipt and message status until transcript observation reconciles them. Slash commands still require an idle terminal. |
| Permission answers by request id | `tool.check` and `tool.call` | Not built. See below. |
| Streaming assistant text | `turn.step` | Not built. See below. |

## Known limits

- **Permissions.** `tool.check` resolves to the engine's verdict before the
  dialog opens, and a hook has no way to settle an open permission dialog as
  allowed. A `tool.call` hook can end the dialog with a denial. Allowing would
  mean abandoning the call and running it again under an approving
  `tool.check`, which is untested. Until that is proven, permissions stay on
  the screen-verified path.
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

## Trust

The bridge adds no authority. A prompt it submits has the standing a typed one
has, and the Hub checks the same interact grant before queuing either. The
inbox is owner-only, and `take` is reachable only by a process that already
runs as the session's user with its `LATCH_SESSION_ID`.
