# Feature: start a Latch session from the mobile app

**Status:** implemented; physical-device behavior not yet field-verified.
**Scope:** Latch Mobile and the protocol-major-2 gateway on the linked Mac.
**Implementation plan:**
[`PLAN_MOBILE_SESSION_CREATION.md`](PLAN_MOBILE_SESSION_CREATION.md).
**Field check:**
[`FIELD_CHECK_MOBILE_SESSION_CREATION.md`](FIELD_CHECK_MOBILE_SESSION_CREATION.md).

## Product outcome

A person away from their desk can open Latch Mobile, choose a folder on their
linked Mac, and start a persistent shell session in that folder. The session
then appears in the ordinary Sessions list. Creating it does not attach the
phone or launch Claude, Codex, or another agent; the person opens the session
and starts whatever they want from its shell.

The Mac remains the execution host. Latch Desktop must be running and the Mac
must be awake, exactly as for listing or opening an existing remote session.

## Experience

The Sessions toolbar gains a **New session** button when the linked Mac
advertises both directory browsing and session creation and the paired phone
has the `control` grant.

Tapping it performs the same device-owner authentication used before opening a
terminal, then presents a folder picker for the Mac. The picker:

- opens at the saved **Default folder**;
- opens at the Mac user's home directory when no default has been saved;
- lists folders only, with navigation to a parent folder;
- shows the current absolute path so similarly named folders are
  distinguishable; and
- has one primary action: **Start session here**.

The picker is a remote browser. It does not use the iOS document picker, which
can see the phone's files and cloud providers but cannot browse the Mac that
will run the process.

After creation succeeds, the picker closes and the Sessions list refreshes
with the new session selected visually at the top. It does not navigate into or
attach to that session. The user can tap the row to open the terminal and type
`claude`, `codex`, or any other command.

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

Only the working directory is supplied by the phone. There are no fields for a
command, agent, name, environment variables, or shell path. This keeps the
mobile action predictable and avoids introducing a second, remotely supplied
launch-manifest interface.

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

An older phone ignores the additive flags and continues to list and open
sessions normally.

## Failure behavior

- If the Mac goes offline while the picker is open, the current folder remains
  visible and retry reconnects through the normal paired route.
- If a folder disappears, the app leaves the picker open and asks the user to
  choose another folder.
- If the create response is lost after the Mac starts the session, retrying the
  same request returns the already-created session rather than creating a
  duplicate.
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

## Out of scope

The first release does not launch an agent, accept an arbitrary command,
create a folder, rename a folder, browse files, search the filesystem, manage
multiple saved locations, wake a sleeping Mac, or create a session when Latch
Desktop is not running.

