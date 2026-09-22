# Feature: start a Latch session from the mobile app

**Status:** implemented; physical-device behavior not yet field-verified.
**Scope:** Latch Mobile and the protocol-major-2 gateway on the linked Mac.
**Implementation plan:**
[`PLAN_MOBILE_SESSION_CREATION.md`](PLAN_MOBILE_SESSION_CREATION.md).
**Field check:**
[`FIELD_CHECK_MOBILE_SESSION_CREATION.md`](FIELD_CHECK_MOBILE_SESSION_CREATION.md).

## Product outcome

A person away from their desk can open Latch Mobile, choose a folder on their
linked Mac, and start a persistent session in that folder: a shell, or —
when the Mac lists it — Claude Code or Codex launched as an agent. The session then
appears in the ordinary Sessions list. Creating it does not attach the phone;
a shell waits for the person to open it, and an agent session appears as a
conversation the person opens to talk to.

The Mac remains the execution host. Latch Desktop must be running and the Mac
must be awake, exactly as for listing or opening an existing remote session.

## Experience

The Sessions toolbar gains a **New session** button when the linked Mac
advertises both directory browsing and session creation and the paired phone
has the `control` grant. When the Mac also lists agents it will launch
(`features.sessionAgents`), the button becomes a menu: **Shell**, then one
entry per agent — **Claude Code** and **Codex**. A Mac that lists none keeps the
one-tap shell button.

Tapping it performs the same device-owner authentication used before opening a
terminal, then presents a folder picker for the Mac. The picker:

- opens at the saved **Default folder**;
- opens at the Mac user's home directory when no default has been saved;
- lists folders only, with navigation to a parent folder;
- shows the current absolute path so similarly named folders are
  distinguishable; and
- has one primary action: **Start session here**, or **Start Claude Code
  here** when an agent was chosen, with the picker titled for it.

The picker is a remote browser. It does not use the iOS document picker, which
can see the phone's files and cloud providers but cannot browse the Mac that
will run the process.

After creation succeeds, the picker closes and the Sessions list refreshes
with the new session selected visually at the top. It does not navigate into or
attach to that session. A shell row opens the terminal; an agent row opens
the conversation because the Mac recorded its agent identity.

Codex transcript binding remains a separate connector gap: a new Codex row
routes to Chat, but its conversation stays in the starting state until Codex
supplies an authoritative source binding. Chat content and send remain
unavailable for that session until the binding is implemented.

## Default folder

Settings gains **New sessions → Default folder**. Selecting the row opens the
same remote folder picker in selection mode. Choosing **Use as default** stores
that canonical Mac path in `UserDefaults`; it is a preference, not a
credential.

The default controls where each new picker begins. Choosing another folder for
one session does not silently replace the default. If the saved folder was
moved, deleted, or became unreadable, the app explains that it is unavailable,
starts at the Mac user's home directory, and lets the person save a new
default.

## What is created

The gateway builds the same shell manifest as `latch shell`: the Mac user's
configured shell, launched as an interactive login shell, inheriting the
gateway's normalized user environment. The first version uses Latch's standard
initial terminal geometry and derives the session name and command label from
the existing metadata rules.

For an agent session, the phone names only the kind (`"agent": "claude"` or
`"agent": "codex"`).
The Mac resolves the executable itself, asking the owner's interactive login
shell first — `claude` is usually on a PATH that only `.zshrc` adds, whether
installed by npm under nvm, by its own installer under `~/.local/bin`, or as
the `~/.claude/local` alias — and falling back to the gateway's PATH and the
installer's known locations. It declares that path and agent identity in the
launch manifest, then Latch prepares the observer and starts the agent through
the owner's login shell. If nothing is found the request is refused as
`agent_unavailable` before anything is accepted, and the phone names the
missing agent.

Beyond the directory and the agent kind, nothing is supplied by the phone.
There are no fields for a command, arguments, name, environment variables, or
shell path. This keeps the mobile action predictable and avoids introducing
a second, remotely supplied launch-manifest interface.

## Permissions and privacy

Directory browsing and session creation both require the paired device's
`control` grant. A manually configured loopback gateway link retains its
existing effective control grant. Latch Mobile also requires a current Face ID,
Touch ID, or device-passcode check before it reveals folder names or creates a
process.

The directory endpoint returns directory names and canonical paths only. It
does not return file names, file contents, metadata, or search results. Paths
and folder listings remain inside the existing end-to-end encrypted paired
tunnel; the control plane and relay never receive them.

The Mac validates the selected path again at creation time. It must resolve to
an existing directory accessible to the current user. Symlinks are resolved to
a canonical path before the folder is displayed or used as `cwd`.

## Compatibility

Gateway discovery adds two optional endpoint flags: `browseDirectories` and
`createSession`. An older Mac omits them, the generated mobile contract decodes
them as unavailable, and the New session control stays hidden with an update
explanation in Settings. The app never probes an undiscovered route.

Agent creation is advertised separately as `features.sessionAgents`, a list
of kinds. A Mac that predates it omits the key, the phone decodes that as
shells only, and the phone never sends `agent` to such a Mac — its contract
closes the request object, so a shell request stays exactly the two fields
it always was. A kind the phone does not know is dropped from the list rather
than failing discovery.

An older phone ignores the additive flags and continues to list and open
sessions normally.

## Failure behavior

- If the Mac goes offline while the picker is open, the current folder remains
  visible and retry reconnects through the normal paired route.
- If a folder disappears, the app leaves the picker open and asks the user to
  choose another folder.
- If the create response is lost after the Mac starts the session, retrying the
  same request returns the already-created session rather than creating a
  duplicate. The request id is bound to the agent as well as the folder: the
  same id asking for a shell where it started Claude is a conflict, not a
  reuse.
- If Claude Code is not installed where the Mac's login shell can find it,
  the request is refused before anything is accepted, and the same id may be
  retried once it is installed.
- Creation failure never attaches to, resizes, or steals another session's
  terminal surface.

## Acceptance criteria

1. A control-granted phone can browse folders on its linked Mac, starting at
   home when it has no saved default.
2. The user can save a default folder, and later New session pickers begin
   there without changing it during one-off selections.
3. **Start session here** creates one persistent interactive login shell with
   the selected canonical path as its working directory.
4. Success refreshes the Sessions list without attaching to the new session.
5. Repeating the same creation request cannot create a duplicate session.
6. Observe and interact grants cannot browse folders or create a session.
7. Older gateways continue to work and do not show a control they cannot
   serve.
8. No directory data reaches the control plane or relay in plaintext.
9. A Mac that lists `claude` or `codex` starts that agent in the chosen folder,
   and the new row opens as a conversation.
10. A Mac that lists no agents is never sent an `agent` field.

## Validation record (2026-09-22)

Verified against a debug `latch serve` in an isolated `LATCH_HOME`, driven
over the same `/v2` contract the phone speaks after its tunnel, plus
Desktop-shaped manifests through `latch create`:

- discovery lists `agent-launch`, `createSession`, and
  `features.sessionAgents = ["claude", "codex"]`;
- shell, Claude, and Codex creation persist the harness marker and list
  `connector` null, `claude`, and `codex`; retrying the same request id
  returns the same session, the same id with another agent is
  `request_id_conflict`, another device is `request_id_foreign`, and
  observe/interact grants are refused;
- a Claude session created either way opens as a conversation, binds through
  `SessionStart`, accepts `send_message`, and streams the assistant reply,
  with no terminal attach and the launch geometry unchanged;
- a legacy shell-wrapped Claude launch stays `connector: null`, so older
  sessions keep opening the terminal until they are recreated.

Two defects surfaced and were fixed: an inherited `LATCH_SESSION_ID` could
override the new session's id when the creator itself ran inside a Latch
session, and the Conversation Hub's action connector never adopted a binding
written after Chat was first opened, so sends were refused while the state
said sending was available.

Not covered: the physical-device run in
[`FIELD_CHECK_MOBILE_SESSION_CREATION.md`](FIELD_CHECK_MOBILE_SESSION_CREATION.md),
and the installed Mac build, which predates these changes.

### Codex transcript binding follow-up (2026-09-22)

A newly launched Codex session now installs a session-scoped `SessionStart`
hook. On the first Chat message, Codex reports its exact `transcript_path` and
`session_id`; Latch binds that source to the Latch session. Until that first
message, Chat allows sending only while the observed Codex composer is empty.
The rollout adapter publishes Codex's completed user and assistant
conversation items and excludes raw setup instructions and environment
context from Chat.

Codex normally asks for terminal review of a new hook definition. Latch's
structured Codex launch uses Codex's per-invocation
`--dangerously-bypass-hook-trust` option so the Latch-authored hook can run
before the first Chat send; this option also bypasses review for other hooks
enabled in that Codex process. It does not grant project trust or change
Codex's sandbox/approval settings.

Verified with a fresh Desktop-shaped `latch create` manifest (120×40) and a
mobile-shaped `POST /v2/sessions` request (80×24), both in an isolated
`LATCH_HOME`. Each session accepted a first message over the conversation
socket, bound a Codex-supplied thread and existing rollout file, showed the
observed user message and assistant reply, and replayed both on a fresh
socket. Both retained their launch geometry and reported
`surfaceAttached: false`; no terminal attachment was used. The Rust suite
(174 unit and 16 integration tests), Desktop package tests (75), and Mobile
package tests passed. This exercises the gateway contract the phone uses;
the installed phone and Mac builds were not replaced or driven on a device.

Codex may show its own project trust prompt in a directory it has not trusted.
In that state there is no empty composer, so Chat correctly keeps sending
disabled until the trust choice is handled. The Latch launch does not grant
project trust on the user's behalf.

## Out of scope

This release does not accept an arbitrary command or agent arguments, launch
agents other than Claude Code and Codex, pass an initial prompt, create a
folder, rename a folder, browse files, search the filesystem, manage multiple
saved locations, wake a sleeping Mac, or create a session when Latch Desktop
is not running.
