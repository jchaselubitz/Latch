# Claude dialog probe

Harness used on 2026-11-23 (Claude Code 2.1.330) to capture the dialog fixtures in
`fixtures/conversation/claude/dialogs/` and to prove which dialogs a hook or hooks
module can answer. Findings: `planning/CLAUDE_REQUESTS_V2.md`.

- `capture.py`: a command hook. Appends its stdin to `$CAP_OUT`. `CAP_MODE` makes it
  answer: `elicit-accept` (form accept with content, URL decline),
  `perm-allow-suggest` (allow + the request's `permission_suggestions`, destination
  overridden by `CAP_DEST`), `plan-approve` (ExitPlanMode allow + `updatedInput`,
  plus `setMode` `CAP_PLAN_MODE` for the session when set).
- `elicit-server.mjs`: dependency-free MCP stdio server with `ask_form`, `ask_url`
  (elicitations) and `approve` (a `--permission-prompt-tool`, delayed by
  `APPROVE_DELAY_MS` so a hook can win the race). Logs JSON-RPC to `$ELICIT_LOG`.
- `oauth-server.mjs`: loopback MCP server that demands OAuth (discovery + DCR).
  Headless sessions do not offer the `authenticate` tool, so it needs an
  interactive run.
- `mod/`: hooks module that approves ExitPlanMode and Bash from
  `classic.PermissionRequest` and logs `classic.Elicitation`,
  `classic.Notification` and `tool.call`. Set `LOG` first.

Run headless with a clean environment so the run does not bind to a Latch session
or load user plugins:

```bash
env -i HOME=$HOME USER=$USER PATH=$PATH TERM=xterm-256color \
  CAP_OUT=$PWD/out.jsonl CAP_MODE=elicit-accept \
  claude -p "Call mcp__probe__ask_form once and report what it returned." \
  --model haiku --setting-sources local --settings settings.json \
  --mcp-config mcp.json --strict-mcp-config --allowedTools mcp__probe__ask_form
```

`settings.json` registers `python3 -I capture.py` for each hook event of interest;
`mcp.json` points a stdio server named `probe` at `node elicit-server.mjs`. Plan
mode needs `--permission-mode plan --permission-prompt-tool mcp__probe__approve`
because plain `-p` removes ExitPlanMode. Dialogs with no hook (computer use, MCP
OAuth, refusal fallback, usage credits) need an interactive session run by a person.
