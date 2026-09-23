# coo:1036.gm0x — AI session opening findings

## Claude opens in Terminal

Two routes can lead there. `SessionPresentation.default` is Terminal, and the
per-device **Session view** preference intentionally controls where a supported
agent session opens. Choosing Chat in Settings changes that preference.

The more persistent problem is that Overlord-created agent sessions currently
have no conversation connector. For example, the recent Claude session
`ses_1a0ca760fc0183d60` has `command_label: "claude"` but no `harness` in its
`meta.json`; `latch list --json` consequently reports `connector: null`. The
current Overlord Codex objective session `ses_1a0ccf58fc1d9c0` has the same
shape. `command_label` is display text, not connector identity. Latch's
`launch_harness` intentionally recognizes a declared `launch.agent` or direct
agent `argv`, not a shell command string.

Overlord's `packages/core/service/latch-launch.ts` currently builds
`argv: [shellPath, '-ilc', commandString]` without `launch.agent`. Latch therefore
cannot install its Claude observer or report a Claude connector for these
sessions. The mobile route correctly opens Terminal for `connector: null`, and
the Terminal → Chat control is unavailable. Existing sessions cannot gain an
observer retroactively. Overlord should use Latch's structured
`launch.agent: "claude" | "codex"` and `launch.login_shell` fields with direct
agent argv for new sessions, preserving its pre-command through the login
shell prelude. That change belongs in the Overlord repository.

## Codex Chat remains on “Opening conversation”

A mobile-created Codex session does have `connector: "codex"` (for example
`ses_1a0ccf149699671`). Codex creates its transcript on the first prompt, so
the Hub truthfully sends `phase: "starting"` before a source binding exists.
When its observed terminal composer is empty, the Hub also sets
`sendMessage.enabled: true` so the phone can send that first prompt. The phone
previously mapped every `starting` state to its loading view and the composer
then disabled Send even when the Hub offered it. This makes first-prompt
binding unreachable from Chat.

The phone now presents a separate Starting state. It permits Send when the Hub
says sending is available and shows the host's reason when it is not, for
example while Codex is waiting at a terminal-only trust prompt. It keeps
Loading for a socket that has not delivered any conversation state. The
per-device Session view preference and the older terminal UI remain separate
from this state mapping; neither is part of the Codex deadlock.

Verification: `ConversationViewStateTests` and `ConversationInputTests` pass;
the `LatchMobile` iOS app scheme builds with code signing disabled. A physical
phone-to-Mac run was not available in this execution environment, so this
confirms the client state path and compilation, not a deployed device result.
