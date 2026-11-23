# Claude Code Mode probe

Harness used on 2026-11-23 (Claude Code 2.1.330) to capture the Code Mode fixtures in
`fixtures/conversation/claude/code-mode/` for coo:1334.w5d6. Findings:
`planning/CLAUDE_REQUESTS_V2.md`, "Code Mode".

Code Mode starts headless when it is asked for both in the process environment and in
`--settings` (the engine accepts a headless opt-in only from flag settings) and the
model is a 5.5-series or Fable 5.2 model on a direct Anthropic login:

```bash
mkdir -p runs/b-mod work && echo "hello from fixture" > work/notes.txt && cd work
env -i HOME=$HOME USER=$USER PATH=$PATH TERM=xterm-256color \
  CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1 \
  CAP_OUT=$PWD/../runs/b-hooks.jsonl MOD_LOG=$PWD/../runs/b-mod MOD_MODE=deny-msg \
  claude -p "Run exactly this in a single REPL call, then report what happened: \
const a = await tools.Bash({command: 'ls'}); const b = await tools.Bash({command: 'touch made.txt'}); \
const c = await tools.Bash({command: 'echo after'}); ({a, b, c})" \
  --model claude-sonnet-5-5 --setting-sources local --settings ../settings.json \
  --plugin-dir ../mod --allowedTools Read "Bash(ls:*)" "Bash(echo:*)" \
  --output-format json > ../runs/b-out.json
```

- `settings.json`: the env opt-in plus `../claude-dialog-probe/capture.py` on
  PreToolUse, PostToolUse, PostToolUseFailure, PermissionRequest, Stop, SessionStart and
  Notification. Run from a directory where that relative path resolves, or edit it.
- `mod/`: hooks module that logs `tool.call`, `tool.check`, `classic.PreToolUse` and
  `classic.PermissionRequest` (one file per event in `$MOD_LOG`). `MOD_MODE` makes the
  permission hook answer Bash: `deny-msg` (deny with a message), `deny-empty` (deny with
  none), `allow`; `call-deny` denies `touch` from `tool.call` instead.
- `sanitize.py`: copies the transcript records Latch reads (no system prompt, account
  or request records) and the hook and module logs into the fixtures, with paths made
  neutral.

Runs used for the fixtures: `repl-parallel` (a, no dialog), `refused-call` (b,
`deny-msg`), `script-error` (e, a script that throws). Runs c (`allow`), d (no answer:
the headless default refuses), f (`deny-empty`) and g (`call-deny`) were observed but
not kept: in every one the refusal hit that call alone and the script went on.
