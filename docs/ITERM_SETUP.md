# iTerm setup — every window is a Latch session

Latch's adoption path is configuration, not a new terminal. Point an iTerm
profile's command at `latch`, and every new window is already a persistent
session. Closing the window leaves the process running; `exit` ends it.
Reattach with `latch attach` (or bare `latch` from inside another Latch
session, which attaches rather than nesting).

The shell Latch starts is a login interactive shell (`zsh -il`, or `$SHELL
-ilc` when a caller supplies a `-c` command), matching iTerm's normal profile
behavior so the same startup files — including `.zshrc` PATH setup such as nvm
— apply. A login-only shell (`-lc` without `-i`) skips `.zshrc`, so wrappers
like `agp` and `ovld` look missing even though they work in iTerm.

No daemon, no account, and no network connection are required.

## Prerequisites

1. Install the `latch` binary somewhere on your `PATH` (for example
   `~/.local/bin/latch`, or wherever you keep personal tools).
2. Confirm it starts quickly:

   ```bash
   latch --version
   latch list
   ```

   Both should return in well under a tenth of a second. Every new window pays
   this cost.

## Create a Latch profile

1. Open **iTerm2 → Settings → Profiles**.
2. Duplicate your usual profile (or create a new one) and name it something
   recognizable — `Latch`, for example.
3. On the **General** tab, under **Command**:
   - Choose **Command** (not Login shell).
   - Set the command to:

     ```bash
     latch
     ```

     If `latch` is not on the `PATH` that iTerm uses for profile commands,
     use the absolute path instead:

     ```bash
     /Users/you/.local/bin/latch
     ```

4. Optionally set the working directory on the same tab so new sessions start
   where you expect (your home directory, or a projects folder).
5. Make this profile the default if you want every new window and tab to be a
   Latch session. Otherwise open it explicitly via **Shell → New Window /
   Tab** with that profile selected.

That is the entire integration. Latch does not install an iTerm plugin, write
a Dynamic Profile plist, or register a service.

## What you should see

| Action | Expected result |
| --- | --- |
| Open a new window with the Latch profile | A shell appears almost instantly; `echo $LATCH_SESSION_ID` prints a `ses_…` id. |
| Close the window (or disconnect) | The session keeps running. `latch list` still shows it. |
| `latch attach <id>` from another window | The screen is restored, including alternate-screen apps mid-run. |
| Type `exit` in the shell | The session ends and disappears from `latch list` after prune, or shows as exited. |
| Run `latch` again inside the session | Attaches to the enclosing session — never creates a session within a session. |
| `latch run -- something` inside a session | Declines with an error rather than nesting. |

## Day-to-day commands

```bash
latch list                  # most recently active first, with idle time
latch attach                # most recent session
latch attach ses_01J…       # a specific session
latch attach --watch NAME   # look without taking input control
latch open ses_01J… --with iterm  # open a new iTerm attachment
latch open ses_01J… --with iterm --as tab   # …as a tab in the current window
latch stop NAME             # end that session's process group only
latch stop --all --yes      # confirm and end every running session
latch prune                 # reclaim exited / lost session directories
```

## Window or tab

`latch open` creates a new iTerm window unless told otherwise. Pass `--as tab`
for one invocation, or set the default once:

```bash
latch config open.behavior new-tab   # or new-window
```

A caller that holds its own window-or-tab preference — Overlord, or Latch
Desktop — should pass `--as` rather than depend on the stored default, so the
two settings cannot disagree. `--as tab` still opens a window when iTerm has
none, because a tab needs a window to live in.

## Nesting

Because the profile runs `latch`, every window exports `LATCH_SESSION_ID` into
the child. Running `latch` again inside that window is ordinary — it happens
whenever a tool or habit would have opened another shell. Latch attaches to
the enclosing session (or declines for `run` / `create`) instead of starting a
nested one.

## From a phone

The same sessions are reachable from an iPhone by SSHing into this Mac over
Tailscale and running `latch attach` — see [`SSH_SETUP.md`](SSH_SETUP.md). That
is a development path rather than a shipped capability: it needs the Mac
SSH-reachable, and M4 is what replaces it.

## Undo

Switch the profile's **Command** back to **Login shell**, or choose a
different default profile. Existing Latch sessions are unaffected; stop or
`exit` them as usual.
