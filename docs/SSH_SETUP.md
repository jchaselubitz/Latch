# Reaching a Latch session from a phone, over SSH

This is a **development path, not a shipped capability.** It works by putting a
terminal on your phone and pointing it at your Mac: SSH in over Tailscale, run
`latch attach`, and you are looking at the session your desk is looking at. No
part of it is Latch networking. Reachability, authentication, and encryption are
entirely SSH's, and every one of them stops working the moment the Mac is not
SSH-reachable.

That is the point. M2's job is to find out whether reaching an agent from a
phone matters *before* anything is built to serve it. **M4 replaces this
wholesale** with a real transport — a relay, an app, no SSH — and when it lands,
nothing in this document is how you do it.

Do not hand this setup to anyone as a feature. It requires an always-on Mac, an
SSH daemon exposed to a private network, and a key on the phone. Use it to learn
whether the phone case is real.

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
- **Mosh off.** Mosh would paper over exactly the transport deaths this
  milestone exists to observe. If you want the phone experience to be good, use
  Mosh; if you want to know whether the kernel survives a hostile link, do not.

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
| `latch attach --retry NAME` | The default for a phone. Reconnects on a dropped socket, with bounded backoff. |
| `latch attach --watch NAME` | Look without taking control. Does not take input control from the desk, and **never resizes the session**. |
| `latch attach --steal NAME` | Take control from an attachment that already holds it. Only when you mean it. |
| `latch list` / `latch inspect NAME` | Which sessions exist, and who holds control. |

### What `--retry` does and does not cover

`--retry` reconnects the *client to the worker* — the Unix socket inside the
Mac. It backs off 250 ms, doubling to a 2 s ceiling, over 5 attempts, and then
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

The session's size is the current controller's size, and it reverts when that
controller leaves (decision D4). Concretely:

1. The desk session is 200 columns.
2. The phone attaches and takes control at 40 columns. The session reflows to
   40, and the agent on it is usable on a phone.
3. The phone disconnects — cleanly, or by walking into a tunnel. Either way the
   session returns to 200 and the desk client's screen is intact.

Two ways to opt out:

- `latch attach --watch` never resizes. Peeking costs the desk nothing.
- `latch resize NAME --cols 200 --rows 50 --pin` freezes the size against
  controller changes entirely.

---

## Sessions that already ended

Attaching to an exited session is not an error. It paints the last screen the
session had, says how it ended, and exits — read-only, with no raw mode
entered and nothing to type at:

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
| Typing does nothing, screen updates fine | You attached `--watch`, or someone else holds control | `latch inspect NAME`; reattach without `--watch`, or `--steal` |
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
- [`M2_FIELD_REPORT.md`](M2_FIELD_REPORT.md) — the dogfooding protocol and its
  results.
- `planning/IMPLEMENTATION_PLAN.md` (M2) — why this milestone exists and what
  replaces it.
