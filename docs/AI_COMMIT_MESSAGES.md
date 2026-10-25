# AI commit-message drafts on the Mac

The Mac gateway can send a bounded, filtered snapshot of **staged** changes to
Google Gemini and return an editable summary and optional body. It never stages,
commits, pushes or runs hooks. Manual commit messages remain available without
provider configuration. The mobile Commit sheet offers explicit Generate/Regenerate actions.

Final local verification and operational acceptance gaps are recorded in
[the comprehensive verification report](verification/ai-commit-2026-10-25/REPORT.md).
The full mobile suite, Git regression suite, real disposable gateway HTTP checks
and normal iOS build pass. Live Gemini and signed helper Keychain access remain
unverified because no credential is configured; the installed CLI predates this
feature. Limited simulator captures do not establish physical-device or VoiceOver
acceptance.

## Set up the Mac

Use the exact `latch` executable selected in Latch Desktop Settings → Latch CLI.
The desktop does not bundle the CLI. It starts the adjacent `latch-remote` helper
with `--gateway-serve --latch-bin <selected-latch>`, and that helper spawns the
selected executable as `latch serve`. These are processes of the logged-in user,
not a root daemon. The gateway, rather than the GUI or helper, reads the key.

Run the following with that absolute executable path substituted for `latch`:

```sh
latch commit-message set-key
latch commit-message status
latch commit-message model
```

`set-key` reads a hidden terminal prompt. Run it again to replace the credential.
For a password-manager pipeline, use `latch commit-message set-key --stdin`; this
reads one line. Do not put a key in shell arguments, command history, environment
variables, model files, or a checked-in file. No key is printed. The dedicated
generic-password Keychain item has service
`dev.cooperativ.latch.commit-message.gemini` and account `api-key` in the user's
default Keychain. There is no plaintext fallback or Overlord configuration.

Remove the saved credential with:

```sh
latch commit-message remove-key
```

The default model is `gemini-3.5-flash-lite`. Set a supported model identifier:

```sh
latch commit-message model gemini-3.5-flash-lite
```

This writes only the non-secret identifier to `$LATCH_HOME/commit-message-model`
(normally `~/.latch/commit-message-model`). The gateway rereads the model and
Keychain credential on every generation request, so changes need neither a
mobile release nor a gateway restart. Use the same `LATCH_HOME` as the desktop
when changing the model; shell environment inheritance is not required.

Keychain access follows the selected CLI's code identity and the user's login
session. Configure with that same executable, while the login Keychain is
unlocked. A different build/signature or a locked Keychain may prevent access.
The gateway and `status` command disable Keychain interaction process-wide: they
fail closed instead of opening a permission dialog. `status` checks local access
only and never contacts Google. If the gateway reports unavailable, check its
capabilities using the existing authenticated connection, compare its selected
CLI path and user with the setup process, then run `set-key` interactively with
that exact executable. Do not broaden Keychain access to all applications.

## Gateway integration

Discover `endpoints.gitCommitMessage == true` separately from
`features.gitCommitMessage.state` (`unconfigured`, `ready`, `unavailable`). Ready
means the local model/key can be read; it does not prove Google accepts the key
or that the project has access to the model. Missing/false support means manual
messages. No model substitution or automatic retry occurs.

`POST /v2/git/commit-message` requires the existing bearer authentication and a
Control grant. Its canonical request/response definitions live in
`schemas/remote-access/v2/git-commit-message.schema.json`. The body contains
`requestId`, an absolute repository `path`, and the reviewed `expected` branch,
HEAD and index from status. The request body is bounded to 8 KiB. Only an explicit
Generate/Regenerate action after a Google source-disclosure explanation may call
this endpoint; do not generate on sheet open, polling or commit submission.

The collector captures a private staged index, excludes unstaged and untracked
content, and filters sensitive paths and detectable credentials. Filtering is
conservative and incomplete; the user must still understand that selected source
is sent to Google. Context is bounded to 64 KiB and includes safe metadata and
distributed patch coverage. Branch names, local roots and request identity are
not sent. Original source identity and omission/coverage metadata accompany the
editable draft. Source is revalidated immediately before sending and after
receiving. No mutation lock is taken or retained across the network call.

The fixed destination is
`https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`.
Only a constrained Gemini identifier can replace `<model>`; URLs and slashes are
rejected. The key is sent in a sensitive `x-goog-api-key` header, never the URL.
Redirects and environment proxies are disabled. Platform TLS verification stays
enabled. The API format follows Google's [generateContent reference](https://ai.google.dev/api/generate-content).
Prompts/source and raw provider failures are never persisted or logged by Latch.
Google still receives the explicitly requested source disclosure.

The request has a 65-second overall generation deadline; context capture has its
existing 30-second limit and the provider has a 5-second connect/30-second total
timeout. Responses are read under a 32 KiB limit. The prompt treats patch content
as untrusted data, asks for an imperative subject of at most 72 characters and
an optional 1–3 short bullet body, and prohibits invented changes or motivation.
Malformed, blocked, truncated, oversized or invalid text is refused. Semantic
truthfulness and imperative grammar still require the user's editable review.

At most one request per device and four globally may be active; each device gets
six admissions per 60 seconds. Local bearer clients share the `local` bucket.
The device table is bounded to 256 recent entries. Limits are in-memory and reset
with the gateway. Rate-limit failures advise waiting 60 seconds. Provider 429,
refusal, timeout, missing configuration, no staged changes and stale source have
typed sanitized errors; provider/model/Keychain errors never expose raw details.

Cancel an admitted request using Control-authorized
`POST /v2/git/commit-message/cancel` with `{"requestId":"<uuid>"}`. Its body is
bounded to 256 bytes. Cancellation is scoped to the authenticated device and
returns 204 even if the request already finished or is unknown. The original
pending request returns typed `cancelled`; its provider future is dropped. This
cannot recall data already transmitted to Google. Clients should cancel before
discarding an in-flight draft and must also reject late responses using their
sheet/repository/text-edit identity. Cancellation is transient and carries no
durable receipt. The client must never apply a result to a newer source snapshot
or overwrite typing made while generation was pending. Committing remains a
separate explicit action through the existing reviewed-index flow.

## Mobile Commit sheet

Generate Message asks for first-use confirmation that selected staged source is
sent to Google. Acceptance is stored on the phone; later Generate/Regenerate
still requires a tap and keeps the Google disclosure visible. No generation
runs when opening the sheet, refreshing status, typing or submitting a commit.
Keys and model settings stay on the Mac. Old gateways and gateways without
usable configuration retain manual Summary/Description entry and commits.

Support comes from the advertised endpoint, independently of readiness. Readiness
uses the current link's discovery, so reconnect after setting a previously missing
key or fixing inaccessible configuration. It is not a live provider-health probe.
The request itself still checks authorization and configuration on the Mac.

A sheet-owned model binds each request to its repository root, branch, HEAD,
index, request ID and text-edit revision. A response is applied only after a
successful status refresh and identity validation. Typing during generation
wins: the response is discarded with a notice, and the existing fields remain.
Cancelling or leaving the sheet invalidates that request locally, cancels the
transport task and best-effort sends the device-scoped cancel request. A late
response cannot populate a dismissed sheet or supersede a later generation.
Cancel cannot recall source already sent, and races with admission remain a
final-verification case; local identity guards do not rely on server cancellation.

The generated subject is limited to 72 characters and the optional body populates
Description. Both stay editable. Coverage warnings identify partial context.
The generated source stays attached to the result when status changes. Commit is
disabled until regeneration or the explicit “I Reviewed the Message and Staged
Files” action. That action records review of the current identity without
rewriting the original generated source. A subsequent source change requires
review again. Unstaged-only edits do not stale a draft. Existing explicit commit
preconditions, reviewed-index, hook and receipt handling remain authoritative.

## Prior objective verification handoff

The earlier implementation objectives used focused fake-provider/collector checks and HTTP smoke checks
for Control authorization, missing configuration, invalid model configuration,
readiness, output parsing, device-scoped limits and cancellation. No live key is
set, replaced, removed or sent to Google in these checks. They do not establish
signed release CLI/helper Keychain ACL access or credential-scoped model access.

Operational acceptance still requires selected release-binary Keychain
setup/replacement/removal and headless gateway access, configured-project model
availability and override pickup, the full phone-to-Mac drafting and explicit
disposable-repository commit flow, and interactive device/accessibility checks.
The final report above records the completed comprehensive automated coverage
and supersedes the earlier objectives' deferred-testing list.


### Mobile integration evidence — coo:1215.6p2w

Focused smoke command:

```sh
swift test --package-path apps/LatchMobile --disable-sandbox --filter GitCommitDraftTests
```

Two model smoke tests pass: editable subject/body population with partial-context
metadata, retaining the original source through stale/manual review, and preserving
both fields when typing occurs while generation is suspended. These are model
fixtures, not a gateway/provider integration or UI acceptance test. No Git mutation
or live provider request is performed by these tests.

The actual iOS app build command is:

```sh
xcodebuild -project apps/LatchMobile/App/LatchMobile.xcodeproj \
  -scheme LatchMobile -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /private/tmp/latch-ai-mobile-build CODE_SIGNING_ALLOWED=NO build
```

The first sandboxed attempts could not access SwiftPM/Xcode caches or clone
packages; authorized reruns use existing normal development access. The build
identified and fixed the missing Settings label for the new draft endpoint; the
final generic simulator build passed for arm64 and x86_64. Existing trailing-closure
and test-concurrency warnings remain outside this feature.
The two tests were rerun after the source-refresh guard adjustment. Repository
boundary and touched-path whitespace checks also pass.

The handoff to coo:1215.0vh2 reserved: full relevant suites; gateway typed-failure and transport
cancellation mocks; dismissal/repository-switch/repeated-generation and refresh
races; older gateway, configuration/readiness refresh, offline and revoked grants;
Dynamic Type, VoiceOver, interactive sheet dismissal and visual acceptance on a
running simulator/device; signed helper Keychain and configured-project live
Gemini/model override flow through an explicit disposable-repository commit.
No simulator was launched, no physical device or live provider was exercised in
this intermediate objective. The generic simulator build proves compilation only.
