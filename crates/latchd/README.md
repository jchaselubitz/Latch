# latchd

Latch's headless session kernel: one daemon per persistent terminal session.

A `latchd` process contains one child in one PTY, keeps a screen model *off*
the live path, and listens on a unix socket where clients either **attach**
(one exclusive raw surface, by steal) or **drive** the session (control verbs
and pushed events, any number concurrent). No central server; no window, tab,
or pane model — presentation belongs to whoever attaches.

See `planning/HEADLESS_KERNEL_PROPOSAL.md` for the architecture and the
decision to build it by default for newly created sessions; `LATCH_KERNEL=tmux`
selects the patched tmux fallback. Existing sessions always route from their
persisted kernel identity, so changing the selector does not migrate or
interrupt them.

```text
latchd run --id ID --socket PATH [--session-dir DIR] --cwd DIR \
           --cols N --rows N [--env K=V]... [--quiet-ms MS] -- PROGRAM [ARGS]...
latchd stat|snapshot|submit|key|events|attach|kill  SOCKET
```

## Security

The kernel holds a hostile program behind a private socket, so it is
reviewed as such. The threat model, the findings of the first review, and
the invariants the tests pin are in `docs/LATCHD_SECURITY.md`; the review
checklist for future changes is the `latchd-security-review` skill under
`.claude/skills/`. Run the adversarial suite with `just security-latchd`
(or `cargo test -p latchd --test security`).
