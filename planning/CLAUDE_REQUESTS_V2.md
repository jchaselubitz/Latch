# Claude requests v2: every dialog and how the phone can answer it

Investigation for coo:1334.da15, 2026-11-23, Claude Code 2.1.330 on macOS.
Implementation follows in coo:1334.tvhe. Option IDs are the coo:1332 ids in
`schemas/remote-access/v2/conversation-item.schema.json`.

## How this was established

- **Live, headless.** `claude -p` in a clean environment (`env -i`, isolated
  settings, no user plugins), with capture command hooks on PreToolUse,
  PostToolUse, PermissionRequest, Notification, Elicitation, ElicitationResult
  and Stop, a probe MCP server that elicits, and a probe hooks module. Each
  answer path below marked *live* was carried out end to end: the hook or
  module answered and the session acted on the answer. The harness is in
  `docs/experiments/claude-dialog-probe`. The fixtures are in
  `fixtures/conversation/claude/dialogs/`.
- **Engine.** The terminal dialogs are not drawn in `-p`, and a Claude session
  cannot be driven through a terminal from here. Their behavior was read from
  the installed engine: the dialog registry, each dialog's result type, the
  notification sender and the session-status writer. The extract is
  `fixtures/conversation/claude/dialogs/terminal-only/engine-dialogs-2.1.330.json`.
- **Not reproduced.** Computer use, MCP OAuth, the refusal model choice and
  usage-credit consent need an interactive session (and, for the last two, a
  refusal or an exhausted plan). The computer-use transcript fixture is
  synthetic but has the same shape as a real Latch-hosted session from
  2026-10-26.

## Decision table

| Dialog | Signal source | Answer path | Options → coo:1332 ids |
| --- | --- | --- | --- |
| Tool permission (Bash, Edit, WebFetch, MCP tool, …) | Bridge `tool.call` (call id) + `classic.PermissionRequest`. Today's path. | **Phone, bridge.** Change in tvhe: return `{decision}` from `classic.PermissionRequest` instead of denying under `tool.call` and running the call again (see below). | Yes → `allow_once`; No → `deny`; No + text → `deny_with_feedback` (`behavior: deny, message`). |
| Permission with suggestions ("Yes, and don't ask again …") | `PermissionRequest.permission_suggestions` (*live*: `addRules` `Bash(<cmd>)` allow `localSettings`). | **Phone, bridge or hook.** Allow with `updatedPermissions` (*live*: destination `session` suppressed the identical next prompt and wrote no settings file). | Suggestion with destination rewritten to `session` → `allow_session`; suggestion as given (`localSettings`/`projectSettings`/`userSettings`) → `allow_always`; label-only fallback when no suggestion is present. A `setMode` suggestion is a connector-defined option labelled with the mode. |
| Plan approval (ExitPlanMode, `permission_exit_plan_mode_v2`) | Bridge `tool.call` (id) + `classic.PermissionRequest` with `tool_input.plan` and `planFilePath` (*live*). The transcript `tool_use` can have `input: {}` (*live*), so take the plan from the hook, not the transcript. | **Phone, bridge** (*live* from a module): `classic.PermissionRequest` returns `{decision: {behavior: allow, updatedInput: tool_input, updatedPermissions: [{type: setMode, mode, destination: session}]}}`. `updatedInput` is required: for a tool that requires user interaction, an allow without it is ignored and the dialog stays. Reject: `{behavior: deny, message}`. | "Yes, manually approve edits" → `approve_plan` (setMode `default`); "Yes, auto-accept edits" → connector `approve_plan_accept_edits` (setMode `acceptEdits`; or `auto`/`bypassPermissions` when the terminal offers those instead); "No, keep planning" + text → `reject_plan_with_feedback`. Terminal only: the "clear context" variants and "refine with Ultraplan". |
| MCP elicitation, form | `Elicitation` hook (`mcp_server_name`, `message`, `mode: form`, `requested_schema`; no `elicitation_id`, no call id) (*live*). The module sees `classic.Elicitation` and the MCP tool's `tool.call` (with id), but cannot accept. | **Phone, command hook.** Add `Elicitation` to the observer plugin. The hook blocks until the phone answers and prints `{hookSpecificOutput: {hookEventName: Elicitation, action, content}}` (*live*: accept with content reached the server). On a timeout or with no phone, print nothing: the terminal dialog then opens. | Submit → `submit_form` (`accept` + content); Decline → `deny` (`decline`); Cancel → `cancel` (`cancel`). |
| MCP elicitation, URL | Same hook with `mode: url`, `url`, `elicitation_id` (*live*). | **Phone, command hook** (*live*: decline). The URL flow completes at the server, so opening it on the phone works. | Open → `open_url` then `accept`; Decline → `deny`; Cancel → `cancel`. |
| Computer use app access (`computer_use_approval`) | Bridge `tool.call` for `mcp__computer-use__request_access` (id, `apps`, `reason`): exact and immediate, but no dialog opens if every app is already granted. After 6 s idle, Notification `permission_prompt` "Claude needs your permission". | **Terminal only.** The dialog is raised inside the tool; no hook. Returning a fake result from `tool.call` would not grant anything in the engine. | `app_access`, `answerable: false`, reason "Allow or deny app access at the Mac". Closes on the `tool_result`. |
| MCP OAuth sign-in | Not a dialog. The model calls `mcp__<server>__authenticate`; its result carries `authUrl` (engine). `/mcp` (a local JSX command) is the manual path. | **Terminal / Mac only.** The callback is a loopback listener on the Mac; in a local session a pasted URL cannot finish the sign-in. Headless sessions do not offer the tool. | `sign_in`, `answerable: false`, URL shown as `details.url` without `open_url`. Closes on the server connecting or the next turn. |
| Model choice after a safety refusal (`refusal_fallback_prompt`) | Notification `permission_prompt` "Session paused" after 6 s; session status `waiting`/"dialog open". | **Terminal only.** No hook for the choice. PreModelSwitch fires only after "retry on fallback" is chosen. | `other`, `answerable: false`, reason "Choose retry on fallback model or edit prompt at the terminal". Engine options: `retry_fallback`, `edit_prompt`. |
| Fable usage-credit consent (`fable_overage_consent_prompt`) | Same: "Session paused", `waiting`/"dialog open". | **Terminal only.** | `other`, `answerable: false`, reason "Continue on usage credits or switch models at the terminal". Engine options: `consent`, `switch_default`, `request_admin`. |
| Sandbox network access (`sandbox_network_access`) | Notification "A sandboxed command needs network access"; status `waitingFor: sandbox request`. | **Terminal only.** | `network`, `answerable: false`, with the host when known. |
| Workspace trust | None. It runs before the session, hooks and modules exist. | **Terminal only**, invisible to Latch. | Not shown. |
| Other registry dialogs (enter plan mode, goal proposal, peer message approval, auto-mode notices, managed-settings review, browser use) | Notification with the registry text (types `permission_prompt` or `agent_needs_input`); session status. | **Terminal only** unless they also raise PermissionRequest (enter plan mode does, through the permission path). | `other`, `answerable: false`, prompt = the notification text. |

## Signals for dialogs that have no answer hook

1. **Notification hook (`classic.Notification` in the bridge).** Every dialog in
   the engine registry that has a `notification` raises the Notification hook
   once the dialog has been shown *and* the terminal has had no input for 6 s.
   Hooks run before any terminal channel, whatever `preferredNotifChannel` says.
   A missing type is `permission_prompt`, so the type alone is coarse:
   computer use, every permission dialog, plan approval and the two "Session
   paused" dialogs all arrive as `permission_prompt`. The message text tells
   them apart ("Session paused", "Claude Code needs your approval for the
   plan", "Claude needs your permission", …). Reliability: good for a phone
   user, whose terminal is idle, but late by 6 s, and late again while someone
   types at the Mac. It never says when the dialog closes, except
   `elicitation_response`, which follows a dialog-answered elicitation
   (*live*).
2. **`~/.claude/sessions/<claude pid>.json`.** The engine writes `status:
   waiting` and `waitingFor` (registry `waitingFor`, else `permission
   prompt`; `input needed` for elicitations; `sandbox request`) as the top
   dialog changes, with no idle delay, and `busy`/`idle` when it closes.
   Latch knows the Claude pid. This is the best *open and close* signal. It
   is not a documented interface, so treat it as advisory and check the
   `version` field.
3. **Close signals by dialog.** For computer use, the `request_access`
   `tool_result`. For refusal and usage-credit dialogs, the next transcript
   record or `turn.complete`. For elicitations, `ElicitationResult` or the
   MCP tool's result. Otherwise, the status file leaving `waiting`.

Recommendation for tvhe: when the bridge reports `classic.Notification` and no
bridge request is pending, open an `answerable: false` request whose kind
comes from the message text (table above), and close it when the status file
leaves `waiting` or the next transcript or turn event arrives. Use the status
file to open such requests early only when the Notification has not arrived
yet. Never type keys for these dialogs.

## Findings that change the bridge design

- **A module can settle a permission dialog itself.** `classic.PermissionRequest`
  may return `{decision: {behavior: allow, updatedInput?, updatedPermissions?}
  | {behavior: deny, message?, interrupt?}}`. In an interactive session the
  engine runs PermissionRequest hooks concurrently with the painted dialog,
  and whichever answers first settles it. That is the same race the bridge
  builds by hand today, but without the second run under a new id, without
  the "rejected" note in the engine log, and with rule-writing and `setMode`
  options. A module's answer ends the chain: the command hooks beneath it
  (including the observer's PermissionRequest record) do not run for that
  dialog (*live*).
- **The current bridge mishandles ExitPlanMode.** Its generic `tool.call` hook
  registers ExitPlanMode like any call and announces it on PermissionRequest.
  A phone "Yes" would deny the original and `$.tool.call` ExitPlanMode again.
  The type kit says a built-in whose dialog is the call itself opens a fresh
  dialog in that case. Exclude ExitPlanMode from the rerun path in tvhe.
- **Elicitation needs a command hook.** In a module, `classic.Elicitation` can
  observe the request or block it (a block becomes `decline`), but cannot
  return `accept` with content: the module result type has no elicitation
  fields, and `next(e)` resolves to `{}` (*live*). Hooks run *before* the
  dialog (engine), so while the hook waits for the phone the terminal shows
  no form. The default command-hook timeout is 600 s. Recommendation: wait
  only while a phone is attached to the session, with a 120 s timeout set in
  `hooks.json`, and show a bridge `ui.status` line ("Answer in Latch, or wait
  for the form here") while waiting. On timeout, print nothing so the terminal
  dialog opens. Never auto-accept. Correlate with the running MCP call through
  the bridge's `tool.call` for an `mcp__<server>__*` tool with the same
  `mcp_server_name`, and with `prompt_id`.
- **Code Mode.** Not exercised here. Settled in coo:1334.w5d6: see "Code
  Mode (REPL)" below.

## Fixtures

`fixtures/conversation/claude/dialogs/<case>/` with a `meta.json` each:

| Case | Contents |
| --- | --- |
| `plan-approval` | Hook payloads; transcripts approved by a hook (with `hook_permission_decision` and `plan_mode_exit` attachments) and rejected; the module's view. |
| `elicitation-form` | Hook payloads answered by a hook and unanswered (`ElicitationResult` + `elicitation_response`); transcripts; MCP wire; the module's view. |
| `elicitation-url` | Hook payload with `url` and `elicitation_id`, transcript, MCP wire. |
| `permission-suggestions` | `permission_suggestions` payload; allow-for-session sequence with no second prompt. |
| `computer-use-app-access` | Synthetic `request_access` call and result in the shape of a real session. |
| `terminal-only` | Engine extract: dialog kinds, options, notification texts, the 6 s rule, the status-file writer, MCP OAuth and trust notes. |

## Implemented in coo:1334.tvhe

Observer plugin v3 and bridge v3 (older launches keep their old hook set; the
connector reads only records that exist).

- **Plan approval and suggestions.** `classic.PermissionRequest` in
  `register.ts` reports `permission.open` with `plan`, `plan_path` and the
  `options` its own hook can carry out, then holds the hook for the phone. An
  answer returns `{decision}` (`updatedInput` + `setMode` for a plan,
  `updatedPermissions` for `allow_session` / `allow_always`, `deny` + message
  for the feedback options). If the call ends another way the hook is released
  and falls through to `next(e)`. ExitPlanMode is never re-run. Plain
  allow/deny still use the existing `tool.call` path.
- **Elicitation.** The `Elicitation` command hook (`latch __conversation-hook`)
  records the request, then blocks up to `LATCH_ELICITATION_WAIT_SECS` (default
  120; hook timeout 135) for an answer file queued by the connector. No answer:
  an `ElicitationTimeout` record, no output, and the terminal form opens; the
  phone request becomes `answerable: false`. Never auto-accepts. Forms are
  checked against `requested_schema` before the answer is queued.
- **Terminal-only.** `Notification` (observer) and the bridge's
  `terminal.open` (computer-use `request_access`, reported only if it outlasts
  1.5 s) raise `answerable: false` requests with a reason. They close on
  `terminal.closed`, `Stop`, `ElicitationResult`, or a replacement. Keys are
  never typed. The status file is not read (advisory, deferred).
- **Attention notifications** already carry `request_kind` (and `appName`);
  the new kinds pass through unchanged.

## Richer activity and session state (coo:1334.m1ng)

Captured live on Claude Code 2.1.330 (headless `claude -p`, capture hooks on
every event). Fixtures: `fixtures/conversation/claude/activity/`.

### What the engine provides

- **Subagents are no longer sidechain records in the main transcript.** Each
  subagent writes `<transcript dir>/<session id>/subagents/agent-<id>.jsonl`
  (`isSidechain: true`, `agentId`) and `agent-<id>.meta.json`, whose
  `toolUseId` names the Agent call that started it. `SubagentStart` carries
  `agent_id` and `agent_type`; `SubagentStop` adds `agent_transcript_path` and
  `last_assistant_message`. Agents launched in the background return
  `status: async_launched` at once; their end reaches the transcript as a
  user record with `origin.kind: task-notification` and a
  `<task-notification>` body (`<task-id>`, `<status>`), which Latch used to
  show as a prompt the person typed. The Agent call's PostToolUse
  `tool_response` is the transcript's `toolUseResult` (`agentId`, `status`,
  `totalTokens`, `totalDurationMs`, `totalToolUseCount`, `resolvedModel`), so
  the observer does not register PostToolUse.
- **Compaction runs as an untyped engine fork**: a `SubagentStop` with
  `agent_type: ""` and no `SubagentStart`. It is not a subagent.
- **Tasks**: `TaskCreated` / `TaskCompleted` (`task_id`, `task_subject`); the
  transcript has the same through `TaskCreate` results (`toolUseResult.task`)
  and `TaskUpdate` inputs (`taskId`, `status`, including `in_progress`, which
  no hook reports).
- **Notices**: `PreCompact` / `PostCompact` (`trigger`, `compact_summary`),
  `StopFailure` (`error`, e.g. `model_not_found`, `rate_limit`; no `Stop`
  follows), `PostModelSwitch` (`from_model`, `to_model`, `source`).
- **State**: every hook carries `permission_mode` and `effort.level` (main
  loop only when `agent_id` is absent); `SessionStart` names the `model`;
  assistant records carry `message.model`; user records `permissionMode`. The
  bridge reads `$.session.usage()` (context tokens, window and percent, rate
  limits, cost) and `$.session.model()`.

### Implemented

- **Observer v4** adds `SubagentStart`, `SubagentStop`, `TaskCreated`,
  `TaskCompleted`, `PreCompact`, `PostCompact`, `StopFailure` and
  `PostModelSwitch`. `PreModelSwitch` is not registered: it can block the
  switch, and a process per switch adds latency for nothing the post event
  lacks. Free-text fields (`last_assistant_message`, `compact_summary`,
  `task_description`, `error_details`, `custom_instructions`) are cut to 4 KiB
  before the sidecar; the record itself stays under `MAX_HOOK_BYTES`.
- **Bridge v4** reports `session.usage` at greeting and after each main-loop
  turn (no polling while idle).
- **Connector** (`connectors/jsonl/activity.rs`):
  - a `subagent` item per subagent, linked to its Agent call
    (`parent_tool_id`, from the meta file, else the oldest unclaimed Agent call
    of its type), sent only after that call so it always follows it; its own
    tool calls are read incrementally from its transcript within the poll
    budget and nested with `parent_subagent_id`, at most 60 shown per subagent
    (the rest counted in its summary); summary = description, final answer
    (2 KiB) and the call's stats. A subagent whose call is never read is shown
    alone once the sources are caught up. Nested subagents nest under their
    parent subagent. At most 32 are tracked.
  - `task-notification` records update their subagent and are no longer shown
    as prompts.
  - one `task_list` item per turn, placed where the transcript first changed
    the list; hooks only refresh it (they are read ahead of the transcript).
    `TodoWrite` fills the same list.
  - `notice` items: compaction (one item, settled by `PostCompact`), a failed
    turn (which also closes the turn with outcome `error`), a model switch.
  - state: `model`, `effort`, `permissionMode` (newest-timestamp wins, so a
    replayed older record cannot undo a newer hook) and the new
    `usage` (`contextTokens`, `contextWindow`, `contextPercent`,
    `rateLimits[]`, `costCents`).
- **Mobile** shows subagents (collapsed, with their calls), task lists and
  notices with the coo:1332 renderers, and a one-line session summary above
  the composer: "Haiku 5.5 · medium effort · Plan mode · 25% context ·
  7-day limit 81% · $0.42".

### Changing model, effort or permission mode from the phone (investigated)

| Option | Finding | Decision |
| --- | --- | --- |
| `/model <alias>` as a prompt | Live in `-p`: `/model sonnet` ran the command, fired Pre/PostModelSwitch with `source: command` (fixture `model-switch`). The phone's composer already offers the advertised slash commands through the bridge's `prompt.submit`, at the Interact grant. In an interactive session the engine's PreModelSwitch path returns `skipConfirm`, so a switch can open a confirmation dialog (cache cost) that only the terminal answers; not observed live. | Not added as a dedicated control. Follow-up. |
| `$.command.run({command: 'model' \| 'effort', args})` from the bridge | The kit's explicit API for the same commands; would allow a typed `set_model` / `set_effort` action gated on the Control grant, with the result reported back. Needs an interactive run to prove the confirmation behaviour. | Follow-up. |
| `PermissionRequest` `updatedPermissions` `setMode` | Works only while a permission dialog is open (proven in tvhe for plan approval and suggestions). Not a general mode switch. | Already used where it applies. |
| `$.config.set` | Sets `/config` rows; the permission mode there is the persisted default, not the live session's mode, and a write changes the person's settings. | Rejected as unsafe. |

## Code Mode (REPL) (coo:1334.w5d6)

Captured live on Claude Code 2.1.330 with Sonnet 5.5, headless. The engine
accepts a headless Code Mode opt-in only from flag settings, so the run sets
`CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1` both in the environment and in
`--settings`. Harness: `docs/experiments/claude-code-mode-probe`. Fixtures:
`fixtures/conversation/claude/code-mode/` (`repl-parallel`, `refused-call`,
`script-error`). An interactive Code Mode session in Latch (`latch run --
claude`) was not run from here (a nested interactive Claude cannot be
driven); the bridge was loaded as a module in every headless run.

### What the engine writes

- **Transcript.** A `repl-mode` record, the assistant `tool_use` named `REPL`
  (`input.code`, optional `input.description`), and its `tool_result` whose
  `toolUseResult` has `code`, `result`, `stdout`, `stderr`, `toolsUsed`, and
  `error` (script threw; `is_error: true`) or `refusedCalls` (a note: `-
  "Bash", refused: "…"` per call) when they apply. While the script runs,
  `host-row` records (`ranByToolUseId`, `block` = the in-script `tool_use` or a
  placeholder `tool_result`) are written, the first ones before the REPL
  `tool_use` record itself. After the result, a virtual assistant/user pair
  per in-script call (`isVirtual`, `ranByToolUseId` on the call, ids
  `toolu_cc_…`) sits on the main chain; a script that throws adds a virtual
  `REPL` pair with id `repl-error-…` and `scriptErrorCopy`.
- **Hooks.** The REPL call raises PostToolUse only (no PreToolUse, no module
  `tool.call`). Each in-script call raises PreToolUse, PostToolUse,
  `tool.call` and `tool.check` with its own `tool_use_id` (`toolu_cc_…`), and
  PermissionRequest when it asks. Stop is unchanged.
- **Refusals.** Every refusal seen refused that call alone and the script went
  on: a PermissionRequest deny with a message, a deny with no message
  ("Permission denied by hook"), the headless default, and a `{deny}` returned
  from the module's `tool.call`. Only the terminal's own "No" without a
  comment stops the whole script (per the Code Mode docs; not reproducible
  headless).

### Decisions (implemented)

- **Connector.** A REPL call is one Tool item named **Code**. Its summary is
  the script's `description` (or "Script"), then, while running, "running
  Bash, Read" from the `host-row` records, and when done "used Bash, Read",
  "N calls refused (Bash)" and "failed: <first error line>". It ends Failed
  when `error` is present or the result is an error. The code is never
  shown. The virtual in-script records add no items; they stay on the branch
  chain, and a task or subagent call inside a script still updates the task
  list or links the subagent, under the Code item. The same naming applies
  to scripts in subagent transcripts. Without `toolUseResult` (a record with
  several results) the facts are read from the result's tagged text.
- **Bridge (v5).** In-script calls are identified by the engine's own id
  convention (`repl_` / `toolu_cc_`, the prefixes the engine itself checks), reported
  with `in_script: true` on `permission.open`, and are answerable from the
  phone. Every answer, including plain Yes and No, is returned from the
  `classic.PermissionRequest` hook: Yes = `{behavior: allow}`, No =
  `{behavior: deny, message: "The person denied this call in Latch."}`, so a
  phone No refuses that call only. The `tool.call` deny and re-run paths are
  not used for them (a re-run would run outside the script). The connector
  adds the reason "A Code Mode script made this call. No refuses this call
  only; the script goes on." when the input gives none.
- **`tool.check` self-approval.** Kept, with a comment: Latch sets no minimum
  Claude version (the bridge was built on 2.1.292), and the approval is
  redundant only from 2.1.321.


## Background work (coo:1334.4s0s)

Captured live (Claude Code 2.1.330, headless; `fixtures/conversation/claude/activity/background-work/`):

- Every `Stop` carries `background_tasks`: `{id, type, status, description}` plus `command`
  (shell, monitor) or `agent_type` (subagent). Seen types: `shell`, `monitor`, `subagent`;
  status is `running` for every listed task.
- `SubagentStop` lists the subagent itself and anything it started, so it is not the
  session's view; the connector ignores it for background work.
- No hook fires when a task ends. The engine injects a task-notification prompt, runs a short
  turn, and that turn's `Stop` lists what is left, down to an empty list. `SessionEnd`
  follows on exit.

Connector: each `Stop` replaces `ConversationState.background_work` (count of unsettled
tasks; kinds in first-seen order, normalised to snake_case). It clears on a later real prompt
(newer than that `Stop`; a task-notification turn keeps it), an empty `Stop`, `SessionEnd`
(observer v5 registers it), a rebind, or when the Hub marks observation unavailable. Persisted
in the runtime checkpoint.

Attention: a turn that ends Idle with work running still sends a completion alert, but the
sealed payload carries `b` (count) and the phone says "Turn finished · N background tasks
still running" instead of "Finished working".
