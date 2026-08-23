# Decision: exclusive attach, last frame, then native paint

**Status:** decided, not implemented.
**Choice:** Latch keeps the session. At most one human control surface is
attached at a time. Attach **steals**. The new surface is given the **current
frame**, then reads and writes the agent's own byte stream.
**Scope:** `latch attach` / `latch open` local viewers; SSH/Termius; later the
gateway terminal WebSocket. Conversation Hub is a different channel and is not
a TUI paint surface.

This records the attach contract that follows from
coo:751 (iTerm parsing tmux's reconstructed CSI is the live-session freeze)
and the follow-up that one surface at a time is enough if steal works.

It does **not** resurrect `latch-term` as a live transcoder. The current frame
is the pane grid the session kernel already keeps. After that frame, Latch must
not sit in the VT path.

---

## Requirements

1. **Latch owns the session.** The child keeps running with no window. Any
   allowed client — desk iTerm, another configured terminal, Termius over SSH,
   a future phone terminal channel — can attach to that same session.
2. **Every attach includes the last frame.** The user must be able to see and
   answer whatever the agent is waiting on (directory trust, a permission
   modal, a stopped composer). That UI is whatever is on the pane **now**, not
   a replay of the session.
3. **After that first frame, read and write performance matches a direct
   agent → TUI paint.** The active terminal parses the bytes the agent wrote,
   the way it does when Claude or Codex is started in that terminal with no
   Latch. Latch must not transcode the live stream through a second screen
   model.

Multiplexing is over **time** (desk, then phone, then desk), not over **space**
(two terminals both receiving a live transcoded view).

---

## What this replaces

Today `latch attach` is `tmux attach-session` with no `-d`. Every viewer is a
tmux client. tmux parses the pane, then emits **its** CSI into iTerm / Termius.
Two clients can be attached; `window-size latest` resizes everyone to the
newest. That is how a phone sees a desk prompt today, and it is also why a
long-running agent TUI makes iTerm's main thread parse a heavier stream than
native Claude.

The exclusive-attach path: Latch (not iTerm, not Termius) is the only client of
the session kernel. Human terminals run `latch attach` and receive (1) one
frame, (2) a splice of pane output and stdin.

Users do not turn on iTerm's tmux control-mode integration, and they do not
point Termius at Latch's private tmux socket. The command stays `latch attach`.
Control mode, if used, is internal to Latch.

---

## Roles

| Role | Owner |
| --- | --- |
| Child process, PTY, current pane grid | Latch session kernel (private tmux is acceptable) |
| Which human tty is live | Latch attach (exclusive, steal) |
| Parsing and painting the TUI | The user's terminal (iTerm, Termius, …) |
| Conversation cards / tool approvals | Conversation Hub (unchanged; not this path) |

The agent has no “paint over here” API. It writes its PTY. Latch points that
PTY at one tty at a time and, on steal, shows that tty the grid first.

---

## Attach–steal flow

The same steps apply to the first attach of a new session and to a steal from
another surface. “Current frame” means the visible pane, including alternate
screen, cursor, and modes — a snapshot of **now**.

### 1. Session exists, maybe with a viewer

Overlord `create` then `open` still opens the configured desk terminal running
`latch attach`. That attach is surface A (usually iTerm). The agent starts
once a viewer exists (existing first-viewer gate). If the user is away, surface
A is still attached at the desk; the pane holds whatever the agent painted
(for example a directory-trust prompt) even if nobody is looking at it.

If there is no viewer yet, Latch still **reads** the PTY so the child cannot
block on stdout, and it keeps the pane grid up to date. Bytes are not stored
as a full-session tape. They update the current frame and are then dropped.

### 2. A second `latch attach` arrives (Termius, another window, …)

This is a steal, not a second live client.

1. **Identify** the session (id or name). Refuse only if the session is gone
   (`lost` / missing). A live session with another surface attached is valid.
2. **Disconnect surface A.** The previous `latch attach` exits. The desk
   terminal is no longer a paint target. It must not remain a tmux client that
   still consumes (and can back-pressure) pane output.
3. **Take the new tty as the only surface** (surface B).
4. **Resize the pane** to surface B's winsize, then `SIGWINCH` the child so a
   TUI that redraws on resize can emit a native frame at the new size.
5. **Send the current frame** to surface B's tty — the grid as of this moment,
   at B's size if the kernel has already reflowed, otherwise the last grid
   (the trust prompt, the permission modal, the composer). This is required
   even if step 4 produced no new output. A blocked directory-trust prompt
   often will not paint again until the user answers; the last frame **is**
   that prompt.
6. **Spliced live I/O.** From this instant, pane stdout/stderr bytes go to
   surface B unchanged. Keystrokes from B go to the pane unchanged. No second
   VT parser in Latch, no tmux `attach-session` CSI rewrite onto B.

### 3. While surface B is live

Performance target: indistinguishable from `claude` started in that same
terminal. Latch is a fd splice (or tmux control-mode `%output` forwarded as
raw pane bytes), not a screen emulator on the hot path.

If B disconnects (window close, SSH drop, `latch attach` exit), the session
stays. Latch keeps reading the PTY and retaining the current frame. The next
attach repeats the flow from step 2.

### 4. Steal back to the desk

iTerm runs `latch attach` again. Termius is disconnected, pane resizes to the
desk, current frame is sent, then native splice. The agent TUI paints for the
desk size after the resize; the user is not required to replay history.

---

## Last frame, not full replay

Attach does **not** replay every PTY byte since `create`. Full-journal replay
is rejected: agent TUIs live on the alternate screen as a sequence of
full-screen paints; replaying them is slow, easy to desynchronize, and still
only the last paint matters.

| Keep | Do not keep as the attach payload |
| --- | --- |
| Current visible grid (and cursor/modes) | The entire raw PTY log |
| Optional: a short primary-screen scrollback tail for shells, later, if wanted | Hours of alt-screen redraws |

Shell scrollback policy, if added, stays a bounded courtesy on attach. It is
not required to satisfy requirement 2. Requirement 2 is the **last frame**.

---

## Worked example: away from desk, directory trust, phone steal

1. Overlord launches on the Mac. iTerm opens with `latch attach`. Claude paints
   “trust this folder?” and waits. Nothing else is written.
2. Ten minutes later, Termius: `latch attach <session>`.
3. Latch steals. iTerm's attach exits. Pane is resized to the phone.
4. Termius receives **that prompt as the current frame** (it is inherently the
   last frame). The user can select and confirm.
5. Further output is Claude's own stream at phone size.

Latch Mobile **chat** is not this path. Folder trust is a TUI, not a Hub
permission card. Phone interaction with that prompt is a terminal attach
(Termius or a future in-app terminal), not the conversation composer.

---

## Read-only attach

`--read-only` is not a second live paint surface. Until this design has a
separate observe path that cannot back-pressure the pane, a read-only attach
either steals as a viewer that cannot write, or is refused while another
surface holds control. Default `latch attach` is control and steals.

---

## What would change this decision

- A product requirement to **mirror** the TUI on two terminals at once. That
  forces a live transcoder again; exclusive steal is then the wrong contract.
- Evidence that a one-shot current-frame dump cannot restore a specific agent
  prompt (wrong modes, alt-screen not in the snapshot). Then the snapshot
  encoder is wrong; the “last frame, not full replay” rule stays.
- A terminal that cannot consume a standard xterm snapshot plus a raw stream.
  Then that terminal needs its own attach helper; the session contract stays.

---

## Implementation notes (for the work that follows)

- Stop using ordinary `tmux attach-session` as the human data path. Keep the
  private tmux server as process/grid kernel if that remains the cheapest way
  to hold a pane and a current frame.
- `latch attach` is the only user-facing attach. Do not document iTerm `-CC`
  against Latch's socket.
- Gateway `/v2/sessions/{id}/terminal` must follow the same contract: steal,
  one frame, then raw pane bytes. It must not `await` unbounded writes in a
  way that blocks the child if that socket is the active surface; if it is the
  active surface and the link is slow, that surface is slow — exclusive steal
  means the desk is already detached.
- `docs/ARCHITECTURE_RULES.md` describes the shipped behavior: one human
  surface, exclusive steal, and `latch-tmux` as a required patched kernel.

Related: [`ATTACHMENT_ARCHITECTURE_REVIEW.md`](../planning/ATTACHMENT_ARCHITECTURE_REVIEW.md)
(tmux vs splice vs control mode),
[`DECISION_SCROLLBACK.md`](DECISION_SCROLLBACK.md) (snapshot ≠ journal replay),
coo:751 investigation in `ai/history/2026-08-22-coo-751-performance-degradation.md`.
