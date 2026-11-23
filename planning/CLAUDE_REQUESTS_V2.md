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
- **Code Mode.** Not exercised here (coo:1334.w5d6). Note for that objective:
  the in-script calls also run the PermissionRequest hook, so the module
  `decision` path above may avoid the missing `tool_use_id` problem.

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
