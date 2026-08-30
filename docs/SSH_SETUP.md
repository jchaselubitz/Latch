# Reaching a Latch session from a phone, over SSH

This is the manual command-line fallback to Latch Desktop's paired Remote
Access. Put a terminal client on your phone, SSH into the Mac over Tailscale,
and run `latch attach`; you are looking at the same session the desk can show.
Reachability, authentication, and encryption are SSH's, and the setup stops
working whenever the Mac is not SSH-reachable.

For the supported paired-device flow, use **Settings → Remote Access** in
Latch Desktop. It provides pairing, device grants, encrypted transport, and
the native mobile client without exposing SSH. This guide remains useful when
you explicitly prefer a terminal client, need a minimal recovery path, or do
not want to run the desktop app.

---

## What you need

- A Mac running the sessions, awake and on the network. **Sleep ends the
  experiment**, not the session — the session survives the Mac sleeping, but you
  cannot reach it while the Mac is asleep.
- `latch` installed on that Mac and on its `PATH` (see
  [`ITERM_SETUP.md`](ITERM_SETUP.md)).
- Tailscale (or any other way to reach the Mac's SSH port from a cell network).
  **Do not port-forward SSH to the public internet for this.** The whole
  security story here is "the Mac is only reachable on a private overlay
  network"; removing that leaves an exposed sshd as the only thing between your
  agent sessions and the internet.
- Termius on iOS, or any SSH client that gives you a real terminal. Termius is
  what this was tested with; Blink and Prompt work the same way.

---

## 1. Make the Mac SSH-reachable

### Enable Remote Login

**System Settings → General → Sharing → Remote Login**, on. Limit access to
your own user rather than "All users".

Confirm it locally first, before involving the phone:

```bash
ssh you@localhost 'echo ok'
```

If that fails, nothing later in this document will work, and the failure is
easier to read here.

### Install Tailscale on both ends

Install Tailscale on the Mac and on the phone, sign both into the same tailnet,
and note the Mac's name:

```bash
tailscale status        # on the Mac
tailscale ip -4         # the 100.x.y.z address to SSH to
```

MagicDNS gives you a name (`macbook.your-tailnet.ts.net`) that is stabler than
the IP; use it if it resolves on the phone.

**Leave Tailscale SSH off.** It is a separate feature that replaces key
authentication with tailnet identity, and it is a second variable in an
experiment that already has enough. Plain sshd over the tailnet is what this
document describes.

### Put the phone's key on the Mac

In Termius: **Keychain → + → Generate key**, Ed25519, and give it a name. Copy
the *public* key out (Termius will export or share it) and append it on the Mac:

```bash
mkdir -p ~/.ssh && chmod 700 ~/.ssh
cat >> ~/.ssh/authorized_keys        # paste the public key, then Ctrl-D
chmod 600 ~/.ssh/authorized_keys
```

Once key auth works, turn password auth off in `/etc/ssh/sshd_config`
(`PasswordAuthentication no`) and restart Remote Login. A phone that can log in
with a key does not need the Mac to accept passwords from anything else on the
tailnet.

---

## 2. Configure Termius

Create a host:

| Field | Value |
| --- | --- |
| Address | the Tailscale name or `100.x.y.z` |
| Port | 22 |
| Username | your macOS username |
| Authentication | the key you generated, not a password |

Then the settings that actually matter for this use, none of which are defaults
worth leaving alone:

- **Terminal type: `xterm-256color`.** Latch's screen model targets xterm
  (`docs/DECISION_XTERM_COMPATIBILITY.md`). A `TERM` the Mac does not recognize
  produces a session whose child program disagrees with the snapshot about what
  is drawable.
- **Keep-alive on**, at whatever the shortest interval Termius offers. Without
  it a cell network's NAT reaps the idle TCP connection and you find out by
  typing into a dead screen.
- **Font size small enough to be worth attaching at.** This is not cosmetic. The
  session reflows to the width of whatever controls it (see *Geometry* below),
  so the font you pick on the phone is the width your desk session takes while
  the phone holds control.
- **Mosh off.** Mosh changes the connection model and is outside the setup
  this guide describes. Use SSH when you want `latch attach`'s normal detach
  and reattach behaviour.

---

## 3. Attach

Connect the host, and then:

```bash
latch list                    # most recently active first
latch attach --retry          # the most recent session
latch attach --retry mysess   # a named one
```

If `latch: command not found`, SSH gave you a shell that did not read the
profile your GUI terminal reads. Use the absolute path
(`~/.local/bin/latch attach`) or add the directory to `~/.zshenv`, which zsh
reads for every shell including non-login ones.

### The flags that matter from a phone

| Command | Use |
| --- | --- |
| `latch attach NAME` | Take the session's terminal. There is no separate steal flag: **every attach steals**. |
| `latch attach --retry NAME` | The default for a phone. Retries an attach that could not start yet, with bounded backoff. |
| `latch list` / `latch inspect NAME` | Which sessions exist, and whether one currently has a surface. |

There is no `--watch` and no read-only attach. A session has exactly one human
surface, so attaching from the phone takes it from the desk, and attaching
again at the desk takes it back. If you only want to see what an agent is
doing without touching it, use Conversation Hub or `latch inspect` — neither
takes the surface.

When your attach is taken, it exits and says so, leaving your terminal
restored:

```
latch: another terminal took this session's surface; run `latch attach` to take it back
```

The exit code says the same thing to a script: `75` stolen, `76` evicted for
not keeping up with output, `77` the session's program exited.

### What `--retry` does and does not cover

`--retry` reconnects the *client to the worker* — the Unix socket inside the
Mac. It backs off from 100 ms, doubling to a 1 s ceiling, over 5 attempts, and then
**gives up and says so** rather than retrying forever. A client that retries
forever is indistinguishable from a frozen one, which is the exact confusion
this milestone exists to remove.

It does **not** survive SSH itself dying. When the tunnel drops, your phone's
shell is gone and `latch attach` went with it; nothing is left running to
retry. You reopen Termius and run `latch attach` again — and that is fine,
because *reconnect is just attach*: there is no resume handshake, no replay, no
sequence numbers. Every attach is a fresh hard reset plus a full screen
snapshot, so the recovery path and the ordinary path are the same code.

This is why backgrounding Termius is unremarkable. iOS will kill the connection
behind your back; reattaching is the steady state, not an error path.

### Reading the status line

The client prints one dim `[latch] …` line per connection-state change, and
that line is the answer to "is this frozen, or is the agent thinking?":

```
[latch] attached to ses_01J… — waiting for the screen
[latch] connection lost: the connection closed — reconnecting, attempt 1 of 5 in 250ms
[latch] reconnected on attempt 2 — restoring the screen
[latch] gave up after 5 reconnection attempts: … — reattach with `latch attach ses_01J…`
```

**The absence of a line is meaningful.** Losses are reported promptly, so a
quiet screen with no status line means the link is alive and the silence
belongs to the agent. Successful attaches erase these lines, because a snapshot
begins with a hard reset.

---

## Geometry: what the phone does to your desk screen

The session is the size of whatever holds its surface, and the size is adopted
as part of the steal, before the first frame is painted. Concretely:

1. The desk session is 200 columns.
2. The phone attaches at 40 columns. The desk attach exits saying it was
   stolen, the pane reflows to 40, and the agent is usable on a phone.
3. You get back to the desk and run `latch attach`. The phone's attach exits
   saying it was stolen, and the pane reflows to 200.

Between step 2 and step 3 the session is headless: it keeps running, keeps
producing output, and keeps its screen. Nothing is lost by having no surface.

To pin a size against this, use
`latch resize NAME --cols 200 --rows 50 --pin`.

## What you actually see

The first thing painted after a steal is the pane's **current screen** — not
scrollback, and not a replay of everything the program ever printed. From then
on your terminal receives the agent's own bytes, unchanged.

That makes the terminal on the other end responsible for interpreting them, so
its dialect has to match what Latch tells programs it is. Latch runs sessions
under a fixed `TERM` (see `latch inspect`); in Termius, use a profile whose
terminal type matches it, leave "report terminal type" alone, and turn off any
setting that rewrites or filters escape sequences. iTerm and Terminal.app need
no special configuration.

---

## Sessions that already ended

Attaching to an exited session is not an error. It paints the last screen the
session had, says how it ended, and exits, with nothing to type at:

```
[latch] ses_01J… exited with status 0; this is its last screen — reclaim it with `latch prune`
```

This is frequently the reason you picked up the phone at all: something finished
while you were away. Exited sessions stay attachable for **24 hours** before
`latch prune` reclaims them (`latch prune --all` overrides). The reasoning is in
[`DECISION_SCROLLBACK.md`](DECISION_SCROLLBACK.md).

`--retry` deliberately does not treat a finished session as a link that might
come back.

---

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Connection times out from cell, works on home Wi-Fi | Tailscale not running on the phone, or the Mac dropped off the tailnet | `tailscale status` on both; check the Mac is awake |
| `latch: command not found` over SSH but not in iTerm | Non-login shell `PATH` | Absolute path, or set `PATH` in `~/.zshenv` |
| Screen is garbled after attaching | `TERM` is not `xterm-256color` | Fix the Termius terminal type; `echo $TERM` to confirm |
| Your attach exited saying it was stolen | Something else took the session's one surface — the phone, the Desktop viewer, or another window | Run `latch attach NAME` again to take it back |
| Your attach exited saying it could not keep up | The terminal stopped reading output — usually a backgrounded app — and was evicted so the session could keep running | Reattach; the session kept going without you |
| Desk session stayed narrow after the phone left | The size was pinned, or the phone still holds an attachment | `latch inspect NAME`; `latch resize NAME --cols … --rows …` |
| Screen frozen with no `[latch]` line | Nothing is wrong with the link — the agent is working | Wait |
| Screen frozen *and* the last line says `gave up` | The worker is gone or the socket is unreachable | `latch list`; `latch doctor` |

`latch doctor` reports permission problems and sessions with neither a live
socket nor an `exit.json`.

---

## Undoing it

Turn off Remote Login, and remove the phone's key from
`~/.ssh/authorized_keys`. Sessions are unaffected — they never knew about SSH.

---

## Related

- [`ITERM_SETUP.md`](ITERM_SETUP.md) — the desk half; every window a session.
- [`DECISION_SCROLLBACK.md`](DECISION_SCROLLBACK.md) — how much history an
  attach carries, and why reattach on a cell link is not a transfer you wait
  through.
- [`DESKTOP.md`](DESKTOP.md) — the paired Remote Access alternative.
- [`M2_FIELD_REPORT.md`](M2_FIELD_REPORT.md) — the historical SSH field report.
