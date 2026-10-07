# Permission probe

An experiment, not part of Latch's build: a Claude Code hooks module that
tries to answer a permission prompt by call id from a `tool.check` and
`tool.call` pair. What it is for, what its tests prove, and how to watch it in
a real session are in [docs/CLAUDE_BRIDGE.md](../../CLAUDE_BRIDGE.md) under
"The permission probe".

```bash
claude plugin validate docs/experiments/claude-permission-probe
claude plugin test docs/experiments/claude-permission-probe
```
