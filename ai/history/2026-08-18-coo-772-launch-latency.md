# coo:772.gc36 — Overlord launches stop waiting on the viewer

Overlord launches were sitting in `launching` for minutes. On the mission
timeline for this objective's own launch, the runner spent 140 s between
"started launching" and "opened the agent launch command"; an earlier launch of
the same mission that fell back to a direct terminal (no Latch) covered the same
span in 1.3 s. The runner spawns `latch create` and `latch open` synchronously
and cannot report the launch until they exit, so anything slow inside them is
launch latency the user sees as a queued objective and a blank window.

## Behavior

- `latch open` no longer waits out the viewer. It returns as soon as a tmux
  client is attached or the viewer launcher exits cleanly, and after 8 s it
  returns `"pending": true` and leaves the viewer to finish presenting. The
  session is already durable at that point, so nothing is lost by returning.
- Viewer diagnostics go to `viewer-open.log` in the session directory instead of
  a pipe, so a failure that happens after `latch open` returns is still readable.
- The first-viewer gate now follows the actual viewer open. `latch open` stamps
  `viewer-open.json` before asking for a window; the launcher waits 3 s
  unannounced (just the create → open handoff) and up to 30 s once a viewer is
  announced, releasing the instant a client attaches. A background launch, where
  no viewer is coming, now waits 3 s instead of 5.
- Attachment observation polls tmux every 100 ms rather than every 20 ms — the
  old interval spawned up to fifty query processes while the client was starting.

## Diagnosis

`create`, `open`, and the launcher each append their phases to
`launch-timings.jsonl` in the session directory; `latch inspect <session> --json`
returns them as `launch_timings`, with an `outcome` on the phases where waiting
and giving up look the same from the duration alone. Comparing `create.total`
and `open.viewer` against the runner's launching → launched interval attributes
the remaining gap to Latch or to Overlord without further guessing.
