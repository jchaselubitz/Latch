# coo:1036.x2r7 — live Claude request resolution, 2026-09-22

## Setup

- Latch `0.2609211207.0` from this checkout, Claude Code `2.1.278` in two isolated Latch sessions at a 100 × 35 PTY size. Claude was in manual permission mode.
- A temporary test in `jsonl/geometry_tests.rs` constructed `JsonlConnector::for_session`, polled the real session source and observer sidecar, then called `Connector::apply` with `ACTION_RESOLVE_REQUEST`. The test was removed after measurement. This exercised the same connector action as mobile Chat, without a paired phone.
- Both sessions were under `/private/tmp/latch-coo1036-repro/`. The attempted Bash command only touched a scratch path; it was denied locally after the refusal measurement.

## Bash permission request

Claude's observer emitted `PermissionRequest` for `Bash`, with `tool_input.description` of `Create empty permission marker file`. The real screen showed:

```text
Bash command

   touch /private/tmp/latch-coo1036-repro/permission-marker.txt
   Create empty permission marker file

Do you want to proceed?
❯ 1. Yes
  2. Yes, and always allow access to /private/tmp/latch-coo1036-repro from this project
  3. Yes, and switch to auto mode · auto mode handles these prompts for you
  4. No

Esc to cancel · Tab to amend
```

The connector projected a pending `Permission` request with prompt `Create empty permission marker file` and choices `['Allow once', 'Deny']`. After loading the live hook into the connector while the prompt was still visible, both `resolve_request('Allow once')` and `resolve_request('Deny')` returned `Refused { reason: 'the requested choice is not identifiable on the current screen' }`. No choice was sent. `screen_contains_request` was true in this measurement, so the failure was the exact numbered-label match in `visible_choice_key`: none of Claude's labels equals either advertised choice. The rendered options also include durable access and auto mode, which the current card does not show.

On the first full connector poll, the mutations contained `Pending` immediately followed by `Dismissed` for this request, leaving no pending request in connector state, despite a subsequent screen snapshot showing the prompt and `screen_contains_request` returning true. This additional dismissal was observed but its timing or cause was not established here. It can prevent the card from staying actionable even before the choice mismatch is reached.

## AskUserQuestion

In a separate live session, Claude opened this screen:

```text
☐ Marker color

Which color should the scratch marker use?

❯ 1. Red
     Use red for the scratch marker. It stands out and reads as a warning or attention color.
  2. Blue
     Use blue for the scratch marker. It is calmer and reads as informational.
  3. Type something.
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
```

The real `PermissionRequest` observer hook fired for tool `AskUserQuestion` with `tool_input.questions[0].question` equal to `Which color should the scratch marker use?` and option labels `Red` and `Blue`. While the question was open, the Claude JSONL transcript contained the user prompt but no assistant `tool_use` record for `AskUserQuestion`. That record and its result appeared only after an answer was submitted. The current connector therefore projected the observer hook as a **Permission** request with prompt `Allow AskUserQuestion?` and `['Allow once', 'Deny']`, then emitted `Dismissed`; it exposed no live `Question` request from the transcript path.

To test the keystroke mechanism separately from that missing live projection, the temporary harness built a `Question` pending request from the **actual hook input** using `claude_question_prompt` and `claude_question_choices`, then called `resolve_request('Red')` against the still-open real screen. `Connector::apply` returned `Accepted`, and Claude subsequently rendered `Which color should the scratch marker use? → Red`. The later transcript `tool_result` also recorded `Red`. Thus a single-select question with a short, exact numbered label can be submitted by the current action mechanism **if** a matching pending request is available; the normal live path did not make one available in this session.

## Consequences for the next objective

- Derive permission choices from the current rendered numbered lines, including the longer policy choices, and only advertise choices that the action can identify on that screen. For this Claude version, `Yes` and `No` are the short approval and denial labels.
- Treat `PermissionRequest` for `AskUserQuestion` as a question, using `tool_input.questions` while the UI is waiting. The transcript `tool_use` arrives too late in this measured session to create the actionable card by itself.
- Investigate the immediate `Pending` → `Dismissed` transition seen on first poll before declaring real phone approval fixed.
- The `1` key submitted `Red` for the measured single-select question. This says nothing about multi-select or free-text answers.
