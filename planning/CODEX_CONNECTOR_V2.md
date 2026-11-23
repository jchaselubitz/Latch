# Codex connector v2: requests and activity

Mission coo:1333, objective 3n1v. Investigated 2026-11-23 on macOS 27.2.1 with
`codex-cli 0.171.0` (Homebrew cask) and the ChatGPT desktop app's bundled Codex
(`Codex Framework` 158.0.8127). Model default `gpt-6.2-sol`.

## Status of the evidence

| Claim | Status |
| --- | --- |
| Rollout record shapes and statistics | Verified on this Mac (322 local rollouts; only record *kinds* and key names were read, no content was copied). |
| App-server protocol, `ServerRequest` list, response shapes | Verified from `codex app-server generate-json-schema --experimental` for 0.171.0. |
| A second client receives and can answer approvals | **Verified live** against a throwaway app-server (`fixtures/conversation/codex/app-server/`). |
| The TUI runs against `codex --remote unix://…` | Verified on macOS with Codex 0.171.0, a disposable workspace, and synthetic model responses. The mobile-facing gateway approved a scratch command and declined the installed Computer Use plugin’s TextEdit request. Terminal cancellation closed its matching card and a late gateway answer was refused. |
| Hook behaviour (`PermissionRequest`, `PreToolUse`) | **Not verified live.** Running hooks needs the trust bypass, which the harness also blocked. Schema and desk research only. |
| Computer use and desktop-app access | The installed Computer Use plugin sends an MCP elicitation with `connector_id: "computer-use"`. Native operating-system prompts can still exist outside this channel. |

## What the rollout contains

Records are `session_meta`, `turn_context`, `event_msg`, `response_item`,
`token_usage_record`, `world_state`, `compacted` and
`inter_agent_communication_metadata`.

- `event_msg` kinds seen across all local sessions: `item_completed`,
  `token_count`, `thread_settings_applied`, `task_started`, `task_complete`,
  `turn_aborted`. **There is no approval, elicitation or user-input request
  event in the rollout.** The rollout is a history, not a request channel.
- `item_completed.item` kinds: `UserMessage`, `AgentMessage` (with `phase`),
  `CommandExecution` (command, cwd, parsed_cmd, status, aggregated_output,
  exit_code, duration), `FileChange` (per-path `unified_diff`, status, stdout,
  stderr), `McpToolCall` (server, tool, arguments, status, result, duration),
  `Reasoning` (summary_text, raw_content), `Extension` (web search with
  results), `ImageView`, `SubAgentActivity`, `CollabAgentToolCall`,
  `ContextCompaction`.
- Items are written **on completion only**. A running tool is invisible in the
  rollout until it ends, so a live "Running" state is impossible from it.
- `task_started` / `task_complete` / `turn_aborted` give turn state
  (`turn_id`, `duration_ms`, `last_agent_message`). `turn_context` records
  `approval_policy`, `approvals_reviewer`, `sandbox_policy`.
- `response_item` carries the raw model calls (`function_call`,
  `custom_tool_call`, `reasoning` with encrypted content, `additional_tools`).
  `request_user_input_async` appears there as a tool name, but only as a call
  record after the fact.
- Plan updates, token usage (`token_count`, `token_usage_record`) and
  compaction (`compacted`, `ContextCompaction`) are recorded.

## App-server findings (live)

- `codex app-server --listen unix://PATH` serves WebSocket over a Unix socket
  (the path is a symlink into `/tmp/codex-daemon-<uid>/<hash>`, mode 0600).
  No bearer token is needed on the Unix socket; file permissions are the
  authentication. `ws://` needs `--remote-auth-token-env` style tokens.
- `codex --remote unix://PATH` starts the TUI as a client of that server.
  Without `--dangerously-bypass-hook-trust` it shows a hook review prompt when
  hooks are configured.
- `initialize` with `capabilities.experimentalApi: true`, then `initialized`.
- `thread/start` with `approvalPolicy` and `sandbox` produced a real
  `item/commandExecution/requestApproval` ServerRequest (fixture).
- A second connection receives `thread/started` and `thread/status/changed`
  immediately, but **no turn events until it calls `thread/resume`**.
  `thread/resume` fails with `no rollout found for thread id …` until the first
  turn has been persisted, so the second client must retry, or discover the
  thread through `thread/loaded/list` after the first user message.
- After `thread/resume`, **both clients receive the ServerRequest** with the
  same JSON-RPC id, and both receive `serverRequest/resolved` when either
  answers. The second client answered `{"decision":"decline"}` and the owner
  client saw the request resolved. Which client wins a simultaneous answer was
  not tested.
- The second client also receives the full activity stream:
  `item/started`, `item/completed`, `item/agentMessage/delta`,
  `turn/started`, `turn/completed`, `thread/tokenUsage/updated`,
  `hook/started`, `hook/completed`. This is richer than the rollout and is live
  (items start before they finish).
- The CLI flag `-a` accepts only `on-request` and `never` in 0.171.0
  (`untrusted` was removed from the flag); the app-server API still accepts
  `untrusted`.
- This Mac's user config sets `approvals_reviewer = "auto_review"`, so some
  requests are answered automatically and never reach any client. Latch must
  not assume every sandbox escalation becomes a request.

## Request type table

Answer shapes are from the 0.171.0 schema. "App-server" means a Latch second
client answering the ServerRequest.

| Request | Source | Answerable how | Notes |
| --- | --- | --- | --- |
| Command approval (`item/commandExecution/requestApproval`) | App-server (live fixture). Hook `PermissionRequest` also sees it. Not in rollout. | App-server: `{decision}` with `accept`, `acceptForSession`, `acceptWithExecpolicyAmendment{execpolicy_amendment}`, network-policy amendment, `decline`, `cancel`. Use the request's own `availableDecisions`. | Fields: command, cwd, reason, `commandActions`, `proposedExecpolicyAmendment`, `networkApprovalContext`, `additionalPermissions`, `approvalId` (distinct for zsh-bridge sub-commands and stdin). Answer by `id` plus `itemId`/`approvalId`. |
| File change approval (`item/fileChange/requestApproval`) | App-server. Hook `PermissionRequest`. | App-server: `accept`, `acceptForSession`, `decline`, `cancel`. | Diff arrives through `item/fileChange/patchUpdated` and the `FileChange` item. |
| Network access escalation | App-server: carried as a command approval with `networkApprovalContext` and network-policy amendment decisions. | App-server, same as command. | Not triggered live; from schema. |
| Permissions (`item/permissions/requestApproval`) | App-server. | App-server: `{permissions: GrantedPermissionProfile, scope: turn|session, strictAutoReview?}`. Granting a subset is possible. | Not triggered live. Needs a richer phone UI (scope picker). |
| User question (`item/tool/requestUserInput`) | App-server. Model-side tool `request_user_input_async` shows only as a call record in the rollout. | App-server: `{answers: {questionId: answer}}`. | Never show `isSecret` answers. Not triggered live. |
| MCP elicitation, form and URL (`mcpServer/elicitation/request`) | App-server. | App-server: `{action: accept|decline|cancel, content}`. URL mode: the phone can open the URL or defer to the terminal. | `openai/userVerification` mode should be marked terminal-only. |
| Connector app tool approval (`codex_apps` MCP tools) | App-server as an MCP elicitation with `codex_approval_kind: "mcp_tool_call"` and connector metadata. | App-server. | Already-approved calls can complete without emitting a request. |
| Browser use | MCP elicitation with `connector_id: "browser-use"`, origin, and advertised persistence. | App-server for supported confirmations. Browser credential flows stay on the Mac. | The installed Browser Use package emits this metadata. |
| Computer use and desktop-app access | MCP elicitation with `connector_id: "computer-use"`, bundle ID, display name, and advertised persistence. | App-server confirmation with the supported session or persistent scope, decline, or cancel. | The installed plugin remembers ordinary app approvals for a session. Native macOS permission dialogs require action on the Mac. |
| Dynamic tool call (`item/tool/call`) | App-server. | Latch does not implement dynamic tools; do not answer. | Informational. |
| Legacy `execCommandApproval`, `applyPatchApproval` | App-server (older clients). | `ReviewDecision`: `approved`, `approved_for_session`, `approved_execpolicy_amendment`, `denied`, `abort`. | Only if a session negotiates the legacy path. |

Mapping to Latch option ids (for the later objective): `accept` and `approved` →
allow_once; `acceptForSession` → allow_session; execpolicy and network
amendments → allow_always with the rule in the label; `decline` → deny (or
deny_with_feedback); `cancel` and `abort` → cancel (interrupts the turn).

## Hooks

Codex exposes `PreModelRequest`, `PostModelRequest`, `PreToolUse`,
`PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`,
`SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop` and
`Interrupt`. `PermissionRequest` output
`hookSpecificOutput.decision.behavior` can be `allow` or `deny`, so a hook can
answer a permission, and a hook that outputs nothing leaves the TUI dialog in
place. Hooks cover command, patch and tool permissions only. They cannot
answer elicitations, user questions or native computer-use prompts, and they
cannot observe requests that Codex answers itself (`auto_review`). Hooks must
be trusted, which today means `--dangerously-bypass-hook-trust`, and that flag
also trusts every other hook on the machine for that process. Hooks are not
verified live (see status).

## Architectures

**A. Rollout plus blocking hooks.** Observation lags (completed-only items, no
running state, no live text), requests are not in the rollout at all, so every
request would need a hook. Covers command, patch and MCP tool permissions
only. A blocking hook holds a Codex turn open on the network. Rejected as the
primary path.

**B. Latch-owned app-server plus a second client (recommended).** Latch starts
`codex app-server --listen unix://<session dir>/codex.sock` per session and
launches the TUI with `codex --remote unix://…`. The TUI stays the owner and
the terminal stays the fallback. Latch connects as a second client,
`thread/resume`s once the thread exists, and reads the live stream and the
ServerRequests, answering by request id. Gives live tools, deltas, turn state,
plans, token usage, subagents and every request type the protocol carries,
and requires no hooks and no trust bypass.

**C. Hybrid (recommended end state).** B for requests and live activity. The
rollout (path from `thread/resume`, or the existing SessionStart hook while
migrating) stays the source for history, reconnect and replay, so an app-server
crash or restart does not lose the conversation. The existing
`__codex-conversation-hook` SessionStart binding keeps working for sessions
that do not use B (for example resumed saved chats until verified).

Recommendation: **C, built on B.** Objective 57hq (activity) and 8xcd
(requests) should both target the app-server client; the rollout reader
continues for history.

## Risks

1. Fresh hosted sessions use the verified `--remote` TUI loop. Saved-chat resume and alternate launch paths keep completed rollout activity.
2. `thread/resume` fails before the first persisted turn; the client must
   retry on `thread/started`/`thread/status/changed` and tolerate a window with
   no requests visible. A request raised in that window is only on the owner.
3. Both clients can answer. Latch must answer by request id, treat
   `serverRequest/resolved` as the close signal (resolved_by terminal when
   Latch did not answer), and handle a late answer being rejected.
4. Latch owning the server makes it a new process to supervise per session:
   start it before the TUI, kill it with the session, keep the socket private
   (0600, inside the session dir). Do not use or touch the user's desktop
   app-server at `~/.codex/app-server-control`.
5. `experimentalApi` is required for several methods; the protocol is marked
   experimental and changed between 0.171 releases (`untrusted` flag removal).
   Pin and probe the version and degrade to rollout plus terminal.
6. `auto_review` and sandbox profiles mean some actions never reach a client.
   The phone must not imply it sees every escalation.
7. Answering approves commands: the Hub must require the Interact grant, and
   the new client path needs a security review before shipping.
8. Computer-use app access may never be answerable remotely; the design must
   show an observed request with its answerability and an attention notification.

## Verification coverage

- Live Mac coverage used a real Codex TUI, the existing Latch gateway, and the
  bundled Computer Use plugin. It checked terminal resolution, a stale answer,
  and the app-access notification in the local spool.
- Synthetic app-server fixture replay covers file changes, network, permissions,
  questions, forms and URLs, MCP app tools, browser, computer use, legacy
  approvals and terminal-only requests.
- Native macOS privacy dialogs have no verified app-server carrier. The paired
  iPhone and APNs delivery path were outside the Mac smoke test.

## Fixtures

- `fixtures/conversation/codex/app-server/command-approval.owner-client.jsonl`
  and `…second-client.jsonl`: a real command-approval exchange captured from a
  throwaway 0.171.0 app-server, paths and host details sanitized. Agent text
  deltas are truncated to three.
- Rollout record shapes are described above rather than stored: real rollouts
  contain private working content and were not copied. The next objective
  should add synthetic rollout fixtures built from these shapes.
- `fixtures/conversation/codex/source-corpus.jsonl` is still the invented
  generic-reader file and is not a model for new fixtures.

## Request handling and security review

Hosted Codex uses its own app-server connection for requests. The append-only
conversation source gets bounded display fields and opaque Latch request IDs.
The live relay retains the raw server RPC ID and the exact allowed result for
each option. The UI presents an advertised session or persistent choice only
where the protocol supplies it. Additional sandbox permissions grant the
displayed profile for the selected turn or session. Unfamiliar profiles can be
declined remotely and reviewed in full on the Mac.

The Hub checks the Interact grant, operation epoch and the currently offered
option. The local answer socket lives under the kernel's owner-only socket
directory, checks same-user credentials at both ends, bounds frames and read
time, and rejects expired selections. The relay checks the live request ID
again before replying. The socket accepts selections only. It cannot forward
raw JSON-RPC or write to the terminal. There is no new permission hook or
hook-trust bypass on this path. Native identities and options come from an
observed Codex ServerRequest. Raw app arguments, secret question prompts and
answers, and device-verification challenges stay out of the request journal.

A request closes on `serverRequest/resolved`. Terminal resolution processed
before a phone answer makes that answer stale. Codex 0.171 sends the same
resolution notification to both clients and omits the winning client. A truly
simultaneous answer may be attributed to the phone if the relay had already sent
its reply, although neither response can affect a later request. Disconnects
dismiss the old request with timeout provenance. A replacement connection can
project requests that Codex replays, each with a new Latch ID. An expired or
interrupted handoff never produces an automatic allow, and a missing
acknowledgement is reported as uncertain. The Mac stays available to answer.
