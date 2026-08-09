# Fixtures

Language-neutral test data. `crates/` and `packages/` are **independent
implementations** of the same protocol; they are kept honest by these files, not
by sharing code. If a change makes one implementation pass and the other fail,
the fixture is doing its job.

A protocol version is unsupported until every shipping client passes the whole
set for that version.

```text
fixtures/
  protocol/          # encoded frames + expected decoded values
  vt/                # recorded PTY byte streams + expected screen models
```

## `protocol/`

Each case is one JSON file. Bytes are lowercase hex so the file is diffable and
readable in a review.

```jsonc
{
  "name": "control-attach-watch",
  "protocol": 1,
  "description": "attach in watch mode at 200x50",
  "frame": {
    "type": "control",          // terminal.output | terminal.input | control
    "encoded": "10000000...",   // the complete frame: tag, u32 big-endian length, payload
    "payload": "86a174...",     // the payload alone, header stripped
    "decoded": {                // omitted for terminal.output / terminal.input
      "t": "attach",
      "protocol": 1,
      "mode": "watch",
      "steal": false,
      "client": { "kind": "cli", "name": "iterm" },
      "size": { "cols": 200, "rows": 50 }
    }
  }
}
```

Both directions are asserted for every case: `encoded` must decode to `decoded`,
and `decoded` must re-encode to `encoded` byte for byte. A codec that is merely
self-consistent passes half of that and is still wrong on the wire.

### Large payloads

A four-megabyte payload written as hex is an eight-megabyte line, which is not
diffable and not reviewable. Such a case replaces `encoded` with
`encoded_parts` (and `payload` with `payload_parts`): an ordered list of runs
concatenated to form the bytes.

```jsonc
"encoded_parts": [
  { "hex": "0100400000" },                              // literal bytes
  { "repeat": { "hex": "61", "count": 4194304 } }       // hex repeated count times
]
```

A case uses one form or the other, never both.

### Canonical encoding

Re-encoding is asserted byte for byte, so the encoding is pinned rather than
merely valid:

- Control payloads are a MessagePack **map with string keys**.
- The `t` discriminator is the first pair; the message's own fields follow in
  the order the protocol lists them, and nested objects likewise.
- Every value uses its **smallest** MessagePack representation.

### Optional fields are three-valued

`session.update` is a merge, so a field being absent and a field being null are
different instructions and a fixture must say which it means. Absent means
"leave the client's value alone"; present-and-null means "clear it". Cases
carrying an optional field state their intent explicitly, and the suite asserts
the decoded value matches the annotation:

```jsonc
"merge": { "state": "absent", "attachments": "absent", "title": "cleared" }
```

`absent` | `set` | `cleared`. Only `title` can be `cleared`.

### Rejection cases

A rejection case carries `reject` instead of `frame`. It names the error, so
both codecs are held to producing the *same* error for the same bytes — not
merely to failing somehow.

```jsonc
{
  "name": "reject-oversized-length",
  "protocol": 1,
  "description": "MAX_FRAME_PAYLOAD + 1 announced, no payload sent",
  "reject": {
    "encoded": "01004000 01",
    "error": "oversized",   // the codec's stable error name
    "layer": "frame"        // frame | control
  }
}
```

`layer: "frame"` means the bytes are not a decodable frame. `layer: "control"`
means the framing is fine and the MessagePack payload inside it is not — the
frame must decode and the control payload must then be refused.

| `error` | Meaning |
| --- | --- |
| `unknown_type` | Tag byte names no known frame type. |
| `oversized` | Announced length exceeds `MAX_FRAME_PAYLOAD`, rejected before allocating. |
| `incomplete` | Buffer ends mid-frame. Mid-stream this means "read more"; at end of stream the frame is dropped, never partially accepted. |
| `trailing_bytes` | Bytes remain after a frame required to be the whole buffer. |
| `malformed_payload` | Control payload is not well-formed MessagePack, or not a map. |
| `missing_discriminator` | Control payload is a map with no `t`. |
| `unknown_message` | `t` names no message this version knows. |
| `invalid_field` | A known message has a missing, mistyped, or out-of-range field. |
| `unsupported_protocol` | `attach` named a version this build does not speak. |

Every one of these ends the connection: a named error to the peer, then a close.
A decoder that accepts one of these is broken even though nothing crashed, and a
decoder that recovers from one is worse — after a length it could not trust,
every later byte offset is a guess, so it desynchronizes the whole stream
instead of dropping one frame.

## `vt/`

Each case is a directory:

```text
vt/claude-code-startup/
  input.bin          # recorded PTY output
  meta.json          # initial size, and any resize applied partway through
  expected.json      # normalized screen model after the whole stream
```

Screens are asserted as normalized models, never as screenshots: cells with
text and attributes, cursor position and visibility, alternate-screen flag,
scroll region, and the modes in effect.

### Recording, not writing

`input.bin` is recorded from a real PTY running the real program, by
`scripts/capture-vt.py`. Re-record one case with `scripts/capture-vt.py <name>`
and all of them with `--all`.

Hand-written escape sequences only test what the author already knew about, and
the sequences that break a reattach are the ones nobody thinks to write down:
synchronized-output brackets, kitty keyboard flags, OSC 8 hyperlinks, mouse
mode combinations. Recording gets those for free. `meta.json` keeps the command
and terminal that produced the bytes, so a case can be regenerated rather than
argued about.

The checked-in recordings were made on Linux with `TERM=xterm-256color`, on a
machine whose child processes hold no Claude or Codex credentials — so the
Claude Code turn ends in a login-expired result frame and Codex startup lands on
its sign-in screen. Both are real full paints from the real binaries and carry
the sequences the suite exists to check; a case whose recording is limited that
way says so in `meta.notes`. Re-recording on an authenticated Mac changes the
bytes and no test.

```jsonc
{
  "name": "resize-midstream",
  "description": "A pager resized from 100x30 to 72x18 partway through.",
  "size": { "cols": 100, "rows": 30 },        // size at the start of the stream
  "resizes": [                                 // applied while replaying, in order
    { "at_byte": 4262, "cols": 72, "rows": 18 }
  ],
  "recorded": { "command": ["..."], "term": "xterm-256color", "platform": "linux" },
  "notes": "Anything a reader needs that the bytes do not say."   // optional
}
```

`at_byte` is the offset the resize actually landed at during the recording, so
replay applies it at the same point in the stream that the program saw it.

### `expected.json` is a partial model

Every key is optional and only what is present is asserted. A recorded Claude
Code screen has 3000 cells, almost all of which are the banner it happens to
have drawn that day; asserting all of them would be a change detector, not a
test. What each case asserts is what that case is *for*.

```jsonc
{
  "description": "What this case is protecting.",
  "size": { "cols": 100, "rows": 30 },
  "alternate_screen": true,
  "cursor": { "row": 27, "col": 2, "visible": true },   // any subset of the three
  "scroll_region": { "top": 0, "bottom": 29 },
  "modes": { "bracketed_paste": true, "mouse_tracking": "any_event" },  // subset
  "title": "✳ Claude Code",                    // null asserts no title was set
  "pen": "default",                            // attributes new output would use
  "rows": { "0": "before the pager", "1": "" }, // exact text, trailing blanks trimmed
  "cells": [                                   // exact cells, including width
    { "row": 1, "col": 5, "text": "你", "width": 2 },
    { "row": 1, "col": 6, "text": "", "width": 0 },     // wide-char continuation
    { "row": 1, "col": 11, "text": "C", "fg": "default", "bold": true }
  ],
  "contains": ["Sign in with Device Code"],    // text present somewhere on screen
  "scrollback": { "len": "at_limit", "newest_text": "49971", "dropped_at_least": 44971 }
}
```

A wide character occupies two cells: the first holds the text with `width: 2`,
the second is a continuation with empty text and `width: 0`. A combining mark
stays in its base character's cell, and the expectation writes the cluster
decomposed exactly as the stream carries it — a precomposed expectation would
quietly assert a normalization the terminal never performed.

Colors are `"default"`, `{ "indexed": 174 }`, or `{ "rgb": [r, g, b] }`.
`mouse_tracking` is `off` | `x10` | `normal` | `button_event` | `any_event`;
`mouse_encoding` is `x11` | `utf8` | `sgr`.

Omitting an assertion is a decision, and a case that omits something surprising
says why in `meta.notes`. Where emulators legitimately differ — how many cells a
ZWJ emoji cluster occupies, for instance — the fixture stays silent rather than
electing a winner by fixture, and the round-trip below carries the case instead.

The **snapshot round-trip** runs over every one of these cases and is the single
most important test in the suite:

```text
feed input.bin -> snapshot() -> replay into a fresh emulator -> assert identical screens
```

If that holds for a real Claude Code session mid-run, reattach is correct. If it
does not, the product does not work on the platform it exists for, because every
mobile app-backgrounding is a detach and a reattach.

## Adding a case

Name the behavior, not the bug number: `alternate-screen-enter-exit`, not
`issue-114`. A case that only one implementation can pass is a bug in the
fixture.
