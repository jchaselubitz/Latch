# Feature: rich conversation view for agent sessions

**Status:** proposed. Scope revised after an implementation review of the
existing Hub, connector, and mobile store; the capabilities that were cut or
reshaped are recorded in
[Hardest challenges](../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md#hardest-challenges).

**Scope:** Latch Mobile is the primary product surface. A minimalist web SDK
follows for embedding the same conversation experience in browser-based
products. Claude is the only conversation connector in the first release; Codex
support is a later phase with its own unsolved observation problem. A native
Latch Desktop surface is optional and explicitly deferred; the host-local
Conversation Hub remains the shared authority for every client.

**Implementation guide:**
[`../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md`](../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md).

## Product outcome

A Claude session running under Latch can be used as a readable conversation
without giving up the real terminal. The conversation view presents the agent's
messages, tool activity, questions, approvals, and progress in forms suited to a
phone or an embedded web panel. A native desktop view may adopt the same model
later. The terminal remains one tap away whenever the structured view is
incomplete or the user needs direct control.

This is a presentation over the existing agent session, not a second agent and
not a chat service hosted by Latch. The process, PTY, transcript, and connector
remain on the user's Mac. The Conversation Hub continues to turn those sources
into one ordered, resumable, agent-neutral projection.

The feature should make the common path feel like a modern coding-agent chat:

- user prompts are easy to distinguish and revisit;
- assistant responses read like documents, including Markdown and code;
- tool activity is visible without overwhelming the response;
- questions and permissions can be answered directly;
- live work, interruption, delivery failure, and reconnection are explicit; and
- switching to the real terminal is always deliberate and predictable.

## Product principles

1. **The Conversation Hub is authoritative.** Clients render Hub items and
   state. They do not parse agent transcripts, scrape terminal text, merge
   competing sources, or infer whether an operation succeeded.
2. **The terminal is the universal fallback.** A supported agent may still emit
   something the conversation model cannot express. The terminal remains
   available without presenting fabricated chat semantics.
3. **Reading chat is non-destructive.** Opening a conversation to observe it
   must not detach the terminal currently open on the Mac, resize the PTY, send
   input, or require owner authentication. Any unavoidable takeover is a
   separate, explicit action.
4. **Agent-specific knowledge stops at the connector.** Claude and Codex fields,
   transcript graphs, hook payloads, screen glyphs, and input conventions do not
   appear in mobile, web SDK, or future desktop presentation code.
5. **Rich data degrades to useful text.** Every enriched message or tool item
   retains a bounded text or summary fallback that an older or simpler client can
   display.
6. **The interface explains uncertainty.** Refused, ambiguous, interrupted,
   stale, and disconnected are different states and must not collapse into a
   generic error or a silently retried operation.
7. **The conversation remains useful at mobile scale.** A long transcript must
   open quickly, remain scrollable while new work arrives, and avoid forcing the
   reader back to the newest message.
8. **An answer must land where it was aimed.** Latch answers a question by
   acting on the agent's own interface. A control is offered only when the host
   can confirm, at the moment of submission, that the action it is about to take
   corresponds to the choice the user saw. Anything the host cannot confirm is
   not offered at all — it is not offered optimistically and then reconciled.

Principle 8 is why several obvious-looking controls are absent from the first
release. It is the constraint most likely to be worth revisiting, and the
alternatives are laid out in the implementation guide.

## Experience

### Opening and switching views

The existing per-device **Session view** preference continues to decide whether
a supported session initially opens as Chat or Terminal. A session without a
conversation connector continues to open in Terminal.

Both screens expose the other view in their navigation controls when it is
available. Switching one session does not change the device preference.

- **Chat → Terminal** is visible in every supported conversation, not only in an
  error fallback.
- **Terminal → Chat** remains available for a session with a connector.
- Opening Chat observes the Conversation Hub without taking the exclusive human
  terminal surface and without an owner-authentication prompt.
- Opening Terminal follows the existing exclusive-attach rules and clearly says
  when it takes the surface from another viewer.

An exited session can still open in Chat and show retained history. Actions that
require a live agent are disabled with an explanation.

### Conversation layout

The transcript is presented as a sequence of turns rather than a list of equally
weighted bubbles.

- A user prompt appears in a compact, trailing bubble with its sent or delivery
  state.
- Assistant prose is full-width document text. It supports paragraphs, lists,
  emphasis, links, inline code, fenced code blocks, and text selection.
- Code blocks preserve whitespace, scroll horizontally when necessary, and have
  a copy action.
- Repository paths appear as ordinary selectable text. Latch has no file-viewing
  surface on mobile, so nothing claims to open one.
- Reasoning or other low-priority narration, when the connector is allowed to
  expose it, is visually quieter than the assistant's answer and can be
  collapsed.
- System notices such as interruption, compaction, recovery, and unsupported
  provider records use neutral status rows rather than pretending to be agent
  prose.
- Message timestamps and copy actions are available without adding persistent
  chrome to every row.

### Tool activity

Tool calls belong to the turn that caused them. They are grouped below the
assistant content instead of appearing as a series of unrelated messages.

While a tool is running, the turn shows one concise live activity line such as
**Reading file**, **Running tests**, or **Editing `ChatView.swift`**. When the
activity settles, it collapses to a summary such as **Ran 4 tools**.

Expanding the activity reveals the individual calls in order:

- tool name and safe human-readable input summary;
- running, succeeded, failed, stopped, or unknown outcome;
- a bounded output or error preview when the connector supplies one;
- changed paths and an optional patch summary for edit operations; and
- copy actions where the referenced content is available locally.

Raw provider payloads, secrets, environment blocks, and unbounded command output
are not placed in the conversation contract. The connector produces a bounded,
sanitized representation.

This is the highest-value part of the feature and the cheapest to build: the
connector already reads every tool call, its input, and its correlated result
from the agent's own transcript, and currently discards all of it.

### Live turn state

The active turn has a single coherent state:

- **Sending** while the user's operation awaits an authoritative result;
- **Working** while the provider has a live turn;
- **Awaiting input** when a question or approval is pending;
- **Completed** when the provider supplies a completion boundary;
- **Interrupted** when the turn is cancelled or the provider reports an abort;
- **Failed** when the turn itself fails; and
- **Connection lost** when the projection is retained but the host is currently
  unreachable.

**Completed** requires a real completion boundary from the provider. Until one
exists, the UI must not synthesize it from silence: a turn with no new output is
**Working**, not finished. The UI may show elapsed time for a working turn, but
elapsed time is display metadata rather than evidence of progress. A lack of new
text must not be described as reasoning or completion.

### Questions and approvals

One pending request is the primary input surface. While it is pending, the
ordinary message composer is replaced by a card tied to the request's stable ID
and revision.

A question card presents:

- the question text and any explanatory context the provider supplies;
- the provider's own decisions, exactly as the agent offers them, including the
  distinction between a one-time approval and a durable policy change when the
  agent offers both as separate decisions; and
- per-choice descriptions when the provider supplies them.

The card offers exactly the decisions the host can carry out. It does not
synthesize a generic Allow/Deny pair over an agent that is actually offering
three differently-worded options, and it does not present a decision the host
cannot confirm it is able to submit.

Submitting, refusing, or cancelling a card always targets the exact request
that was rendered. If the request becomes stale — the agent moved on, the prompt
changed, or the decision can no longer be matched — the Hub refuses the
operation with a specific reason and the client refreshes rather than applying
the answer to a newer prompt. A refusal here is a normal, expected outcome and
must read as one.

Multi-select answers and free-text answers are **not** in this release. Both are
blocked by principle 8 rather than by rendering work; see the implementation
guide.

### Composer

The normal composer supports multi-line text and preserves a draft per session.
It remains editable during temporary connection loss; sending is disabled with
the host-provided reason.

Sending is also unavailable while a request is pending or while the agent is
mid-tool. Those are agent states, not errors, and are explained as such.

The send control changes to **Stop** only when the Hub has authoritative
evidence of a cancellable live turn.

### Attachments by workspace file handoff

The mobile composer attaches files by **workspace file handoff**: the file is
uploaded to the host, placed in the workspace, and referenced by path in the
message the agent receives. The conversation channel itself carries no file
bytes, and no connector changes.

- The gateway serves `POST /v2/sessions/{id}/attachments?name=…` at the
  interact grant — the composer's grant — and advertises it as
  `endpoints.attachments` with its size limit in
  `features.attachmentMaxBytes` (25 MB).
- The body is the raw file, streamed to disk. It lands in the session's
  recorded working directory under `.latch-attachments/`, which holds a
  `.gitignore` so uploads stay out of the repository. The gateway chooses the
  final name: the suggestion is reduced to `[A-Za-z0-9._-]` and made unique,
  and the file is never written through a symlink or over an existing file.
  The answer carries the absolute path.
- The phone uploads every pending file first and sends the message only once
  all of them have landed, with the paths appended as plain text
  (`Attached file: /…/.latch-attachments/photo.jpg`). A failure at any step
  sends nothing, returns the text to the draft, and keeps the files; files the
  Mac already has are not uploaded again on retry.
- Photos are sent as JPEG, reduced to 2048 px on the long edge, because agent
  image tools read JPEG and not HEIC.
- The "+" menu shows Photo library, Take photo (when the device has a camera),
  and File only when the Mac advertises the route and the device holds the
  interact grant.

A future enhanced composer may also expose:

- **dictation** on mobile, which is an OS keyboard capability and costs nothing;
  and
- **advertised agent commands**, where the connector publishes a bounded catalog
  of commands it knows the installed agent accepts, and the client sends one as
  an ordinary message rather than emulating a menu.

These are the shapes that fit the architecture. Inline image attachments,
`@`-completion backed by live workspace search, and a provider-neutral model or
reasoning-option picker are not in this release; the implementation guide
records why and what they would cost.

Controls appear only when the connector and gateway advertise the corresponding
capability. They never emulate an unsupported command by typing unverified
keystrokes into the terminal.

### History and scrolling

Opening a conversation immediately displays the bounded local cache while the
store resumes from its last generation and revision.

- Earlier history is loaded in pages without moving the row the reader was
  looking at.
- Loading a page of earlier history actually retains it. The client's memory
  bound governs what is *rendered*, not what is *kept*, so scrolling back does
  not discard the page that was just fetched.
- New content follows automatically only while the reader is already near the
  bottom.
- Scrolling upward releases bottom-following and reveals a **Jump to latest**
  control.
- A streaming or repeatedly upserted tail row does not cause settled rows to be
  rebuilt, re-encoded, or re-persisted.
- Large conversations keep bounded memory and render only the rows needed for
  the current viewport.

### Delivery and reconnection

The current operation model remains visible in user terms:

- an optimistic prompt appears immediately after send;
- acceptance keeps it visible until the canonical observed item replaces it;
- refusal returns the exact draft and explains why it was refused;
- ambiguity says that the message may have been delivered and is never retried
  automatically;
- a changed operation epoch moves unresolved work to manual review; and
- reconnect resumes by revision or accepts a replacement snapshot without
  blanking an already settled transcript.

The same conversation should look stable while the phone changes between LAN,
direct remote, and relay paths.

### Unknown and future content

A client that receives conversation data it does not understand degrades; it
never disconnects. An unrecognized item kind, an unknown rich block, or a
malformed optional field results in that item rendering its text fallback or a
neutral placeholder row. It must not fail the enclosing batch, drop the socket,
or put the client into a reconnect loop.

## Surfaces

The intended delivery order is normative: complete the mobile experience first,
then expose the reusable conversation contract and renderer through a small web
SDK. A native desktop conversation surface can follow later and is not a release
dependency for either of the first two surfaces.

### Latch Mobile

Mobile is the first complete surface. It already owns the session presentation
preference, terminal fallback, Conversation Store, and paired transport. The
rich view replaces the current minimal rows without moving transcript folding or
interaction policy into SwiftUI.

### Embedded web SDK

The second surface is a minimalist browser SDK for products that want to embed a
Latch conversation without recreating protocol, reconciliation, or rendering
behavior. It has two deliberately small layers:

- a headless TypeScript conversation client, built on `@latch/client`, that owns
  the conversation socket, revision resume, history paging, operation
  correlation, and the normalized presentation state; and
- an optional React package that renders the standard conversation, tool
  disclosures, request cards, composer, delivery states, and reconnect states.

The embedder supplies the gateway connection, authorization token, session ID,
outer navigation, layout, and theme. The SDK supplies accessible defaults plus
small styling hooks; it does not become an application shell or require the
embedder to use Latch's mobile navigation.

The web SDK is a client-side embedding surface, not a hosted transcript service
or a server-side agent runtime. It does not parse provider files, scrape a
terminal, persist conversation content in the embedding service, or bypass Hub
capabilities. A non-React product can use the headless client and provide its own
renderer without implementing the wire protocol itself.

### Latch Desktop

Latch Desktop may later expose the same conversation as a native session detail
or separate window. This is a later, optional surface rather than part of the
mobile-and-SDK release. It does not need to embed a terminal emulator. Its
terminal escape hatch remains **Open in Terminal**, **Open in iTerm**, or **Open
in Ghostty**, using the user's existing preference.

Desktop consumes the same Hub contract as mobile; it does not read
`~/.latch`, agent transcripts, or connector sidecars directly.

## Permissions and privacy

- Observing a conversation requires the existing observe grant, and nothing
  more. Reading a conversation must not require the terminal grant or an owner
  authentication prompt.
- Sending a message, answering a request, cancelling a turn, handing over a
  file, or invoking an advertised command requires the grant named by that
  action's descriptor and is enforced by the Hub.
- No client treats a locally displayed capability as authorization.
- Conversation data remains inside the end-to-end encrypted paired path. The
  control plane and relay receive no transcript, tool output, path, or prompt
  plaintext.
- The on-device conversation cache uses iOS
  `FileProtectionType.completeUntilFirstUserAuthentication` and is excluded
  from device backup. This is the strictest class compatible with cache writes
  during background reconnect after the device has locked: `.complete` would
  reject them, while `.completeUnlessOpen` does not cover a journal opened
  after locking. Rich items put tool output, file paths, and patch text into
  that cache, so it is treated as sensitive local data rather than incidental
  state.
- Tool input, tool output, patches, paths, and uploads are bounded and
  sanitized on the host before crossing the public contract.
- Agent-owned transcript files remain read-only inputs.

## Accessibility

- Every state is conveyed with text as well as color or animation.
- Reduced-motion settings suppress pulsing and animated auto-scroll.
- Tool disclosures, copy actions, question choices, approval decisions, Send,
  Stop, and Jump to latest have explicit accessibility labels and focus order.
- Dynamic Type can enlarge prose without making code unreadable or hiding the
  composer controls.
- A hardware keyboard can move through the transcript, focus the composer,
  submit a message, and dismiss a question card without a touch-only gesture.

## Compatibility and contract evolution

The first visual improvements use the existing protocol-major-2 item model.
Rich blocks are then added as optional, capability-advertised fields while the
existing `text` and `summary` values remain required fallbacks.

Forward-compatible decoding is a property of the *shipped* client, not of the
change that introduces rich fields. Tolerant decoding therefore ships in the
first release — before any host emits an enriched item — so that the builds
already installed when the rich contract lands degrade instead of failing.

Before shipping the additive evolution, fixtures must prove that the prior
decoders ignore the newly allowed properties. If any existing decoder rejects
them, the enriched model moves behind a new protocol major instead of shipping a
partially compatible contract.

New clients skip unknown block types and render the item's fallback text. The
Hub remains responsible for ordering, grouping identifiers, revisions,
generations, and operation outcomes.

## Acceptance criteria

1. Assistant Markdown, lists, inline code, and fenced code blocks render
   readably and remain selectable.
2. A turn containing several tools renders one compact activity summary and an
   ordered expandable detail view, including failed tools and bounded result
   previews.
3. A pending question or approval replaces the ordinary composer, presents the
   agent's own decisions, and can only resolve the request revision it displays.
   A stale or unmatchable decision is refused with a specific reason.
4. Opening Chat does not detach or resize a terminal already open on the Mac,
   and does not prompt for owner authentication.
5. Chat and Terminal each offer the other view when the session supports it.
6. Reading older history is not interrupted by arriving messages; Jump to
   latest restores following; a fetched history page is retained rather than
   immediately evicted.
7. Refused and ambiguous sends remain visibly distinct and neither is retried
   without a new explicit operation.
8. Disconnecting retains the settled transcript and draft; reconnect resumes
   without duplicates or a blank intermediate screen.
9. The public conversation vocabulary contains no provider conditionals in any
   view. The single shipping connector must not be able to leak its identity
   into presentation code.
10. An observe-only device can read the rich conversation but cannot execute
    any interaction.
11. An unknown item kind, unknown rich block, or malformed optional field
    degrades to a fallback row and never drops or loops the socket.
12. Long-session tests demonstrate bounded memory, stable scrolling, and no
    transcript-wide work per append on either the host or the client.
13. The headless web client can open, resume, page, send, and resolve requests
    without importing React or provider-specific code.
14. The optional web renderer presents the same ordered items, grouped tools,
    pending request, and operation outcomes as the mobile fixture.
15. Embedding the web renderer requires only connection/session inputs and
    optional theme hooks; it does not require a Latch-specific application shell
    or a server-side transcript copy.

## Out of scope for the first rich release

Deferred because the architecture does not currently support them. Each has a
recorded blocker and candidate alternatives in
[Hardest challenges](../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md#hardest-challenges):

- **Multi-select answers** to an agent question.
- **Free-text answers** to an agent question.
- **Inline image or file attachments** sent into the agent as attachments.
  Workspace file handoff by path is the shape that fits, and is what the
  mobile composer implements.
- **A provider-neutral model or reasoning-option picker.** An advertised command
  catalog is the shape that fits.
- **`@`-completion backed by live workspace search.**
- **Codex conversation support.** Codex has no observation mechanism today; it
  is a later phase, not a parity checkbox.
- **Opening a repository path in a Latch file surface**, because no such surface
  exists.

Out of scope on product grounds rather than technical ones:

- A hosted or cloud-synchronized transcript store.
- Semantic chat extraction from arbitrary shell output.
- Editing files directly inside the mobile conversation.
- Full source-control review or pull-request workflows.
- Search across every historical Latch conversation.
- Rendering raw chain-of-thought that the provider does not intentionally expose.
- Replacing the user's preferred desktop terminal with an embedded emulator.
- A hosted multi-tenant chat backend or server-side transcript mirror.
- Requiring a native desktop conversation surface before mobile and the web SDK
  can ship.
