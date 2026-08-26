# latchd

Latch's headless session kernel: one daemon per persistent terminal session.

A `latchd` process contains one child in one PTY, keeps a screen model *off*
the live path, and listens on a unix socket where clients either **attach**
(one exclusive raw surface, by steal) or **drive** the session (control verbs
and pushed events, any number concurrent). No central server; no window, tab,
or pane model — presentation belongs to whoever attaches.

See `planning/HEADLESS_KERNEL_PROPOSAL.md` for the architecture and the
decision to build it. `latch` selects this kernel with `LATCH_KERNEL=latchd`;
the default remains the patched tmux until the daemon has soaked.

```text
latchd run --id ID --socket PATH [--session-dir DIR] --cwd DIR \
           --cols N --rows N [--env K=V]... [--quiet-ms MS] -- PROGRAM [ARGS]...
latchd stat|snapshot|submit|key|events|attach|kill  SOCKET
```
