// Latch's bridge into a Claude Code session it hosts.
//
// The Conversation Hub observes Claude from the outside: it tails the
// transcript and reads the terminal. This module is the inside half. It tells
// the Hub what only the engine knows (a turn began, a turn ended and why,
// which commands the session accepts) and carries out what the Hub asks of the
// session (submit this prompt, stop this turn, answer this question) through
// the engine's own calls instead of keystrokes.
//
// A hooks module has no file or socket API of its own, so every exchange with
// Latch is one run of the `latch` binary that launched the session:
//
//   latch __conversation-bridge hello   record on stdin, answers { inbox }
//   latch __conversation-bridge event   record on stdin, appended to the sidecar
//   latch __conversation-bridge take    answers the queued commands, once each
//
// Outside a Latch session (no LATCH_SESSION_ID) the module does nothing.

import type { Register } from 'claude-code'

import { BRIDGE_VERSION, LATCH_BIN } from './config'

/** How often the inbox directory is looked at. A look is one directory read. */
const POLL_MS = 500
/** How long a submitted prompt may take to enter before it is reported queued. */
const SUBMIT_SETTLE_MS = 2000
const RUN_TIMEOUT_MS = 5000
const MAX_COMMANDS = 200
const MAX_DESCRIPTION = 160
const MAX_QUESTION_TEXT = 1000

type Command = {
  id?: unknown
  kind?: unknown
  text?: unknown
  tool_use_id?: unknown
  answers?: unknown
}
type Answers = Record<string, string>
/** A question the model asked that is still open at the terminal. */
type OpenQuestion = { asked: readonly string[]; answer: (answers: Answers) => void }
type Outcome = { outcome: 'accepted' | 'queued' | 'refused'; detail?: string }
type Host = Parameters<Parameters<Parameters<Register>[0]>[2]>[0]

const reason = (error: unknown): string =>
  (error instanceof Error ? error.message : String(error)).slice(0, 300)

/** What one loaded copy of the module remembers. A reload starts it over. */
type Bridge = {
  inbox: string | undefined
  runningTurn: string | undefined
  isDraining: boolean
  /** Open AskUserQuestion calls by their tool_use_id. */
  questions: Map<string, OpenQuestion>
}

function latch($: Host, action: string, record?: unknown) {
  return $.process.run([LATCH_BIN, '__conversation-bridge', action], {
    stdin: record === undefined ? '' : JSON.stringify(record),
    timeoutMs: RUN_TIMEOUT_MS,
  })
}

/** Reports one fact to the Hub. Observation never fails the session. */
async function report(
  $: Host,
  bridge: Bridge,
  event: string,
  fields: Record<string, unknown> = {},
): Promise<void> {
  if (bridge.inbox === undefined) {
    return
  }

  try {
    await latch($, 'event', {
      bridge_event: event,
      bridge_version: BRIDGE_VERSION,
      // Milliseconds, as the transcript's own records carry: the Hub orders
      // a turn's close against the prompts around it by this.
      timestamp: new Date(await $.clock.now()).toISOString(),
      ...fields,
    })
  } catch {
    // The sidecar is an observation aid; the turn goes on without it.
  }
}

/**
 * Submits what the person wrote in Latch's chat as their own words: the model
 * reads it bare, and the transcript still names this plugin as the route.
 */
async function submitPrompt($: Host, text: string): Promise<Outcome> {
  const entered: Promise<Outcome> = $.prompt.submit({ text, asUser: true }).then(
    answer =>
      answer.drop === undefined
        ? { outcome: 'accepted' }
        : { outcome: 'refused', detail: answer.drop.slice(0, 300) },
    error => ({ outcome: 'refused', detail: reason(error) }),
  )
  const waited: Promise<Outcome> = $.clock
    .sleep(SUBMIT_SETTLE_MS)
    .then(() => ({ outcome: 'queued' }))

  // A prompt submitted while a turn runs enters when that turn ends. The
  // engine holds it either way, so the Hub is told it is queued, not lost.
  return Promise.race([entered, waited])
}

async function abortTurn($: Host, bridge: Bridge): Promise<Outcome> {
  if (bridge.runningTurn === undefined) {
    return { outcome: 'refused', detail: 'no turn is running' }
  }

  await $.turn.abort({ turnId: bridge.runningTurn })

  return { outcome: 'accepted' }
}

type Asked = {
  question: string
  header: string
  multiSelect: boolean
  options: readonly { label: string; description?: string }[]
}

/** The question as the Hub presents it: every label whole, the prose bounded. */
function describeQuestions(questions: readonly Asked[]) {
  return questions.map(asked => ({
    question: asked.question.slice(0, MAX_QUESTION_TEXT),
    header: asked.header.slice(0, MAX_DESCRIPTION),
    multi_select: asked.multiSelect,
    options: asked.options.map(option => ({
      label: option.label.slice(0, MAX_QUESTION_TEXT),
      description: option.description?.slice(0, MAX_QUESTION_TEXT),
    })),
  }))
}

/**
 * Answers an open question for the person. The answer is aimed at one call by
 * its id, so it cannot land on a question other than the one they were shown.
 */
function answerQuestion(bridge: Bridge, command: Command): Outcome {
  const pending =
    typeof command.tool_use_id === 'string' ? bridge.questions.get(command.tool_use_id) : undefined

  if (pending === undefined) {
    return { outcome: 'refused', detail: 'the question is no longer open' }
  }

  const answers = command.answers
  const entries =
    typeof answers === 'object' && answers !== null ? Object.entries(answers) : []
  const isAnswer =
    entries.length > 0 &&
    entries.every(([question, answer]) => pending.asked.includes(question) && typeof answer === 'string')

  if (!isAnswer) {
    return { outcome: 'refused', detail: 'the answer does not match the question asked' }
  }

  pending.answer(Object.fromEntries(entries) as Answers)

  return { outcome: 'accepted' }
}

async function carryOut($: Host, bridge: Bridge, command: Command): Promise<Outcome> {
  if (command.kind === 'submit_prompt') {
    return typeof command.text === 'string' && command.text !== ''
      ? submitPrompt($, command.text)
      : { outcome: 'refused', detail: 'the prompt is empty' }
  }

  if (command.kind === 'abort_turn') {
    return abortTurn($, bridge)
  }

  if (command.kind === 'answer_question') {
    return answerQuestion(bridge, command)
  }

  return { outcome: 'refused', detail: 'this bridge does not know the command' }
}

async function drain($: Host, bridge: Bridge): Promise<void> {
  if (bridge.isDraining || bridge.inbox === undefined) {
    return
  }

  bridge.isDraining = true

  try {
    const waiting = await $.fs.list(bridge.inbox)

    if (!waiting.some(entry => entry.name.endsWith('.json'))) {
      return
    }

    const taken = await latch($, 'take')
    const commands: unknown = JSON.parse(taken.stdout)

    for (const command of Array.isArray(commands) ? (commands as Command[]) : []) {
      let result: Outcome

      try {
        result = await carryOut($, bridge, command)
      } catch (error) {
        result = { outcome: 'refused', detail: reason(error) }
      }

      await report($, bridge, 'command.result', { command_id: command.id, ...result })
    }
  } catch {
    // A missing inbox or a failed run is retried at the next period.
  } finally {
    bridge.isDraining = false
  }
}

async function describeSession($: Host): Promise<Record<string, unknown>> {
  const facts: Record<string, unknown> = {
    bridge_version: BRIDGE_VERSION,
    capabilities: [
      'turn_boundaries',
      'submit_prompt',
      'abort_turn',
      'answer_question',
      'command_catalog',
    ],
  }

  try {
    facts.claude_version = (await $.session.version()).version
    facts.model = await $.session.model()
    facts.commands = (await $.command.list()).slice(0, MAX_COMMANDS).map(command => ({
      name: command.name,
      description: command.description.slice(0, MAX_DESCRIPTION),
      source: command.source,
    }))
  } catch {
    // The greeting still announces the bridge without the optional facts.
  }

  return facts
}

/** Greets Latch and, once it answers where commands are queued, starts looking. */
async function open($: Host, bridge: Bridge): Promise<void> {
  try {
    const greeted = await latch($, 'hello', await describeSession($))
    const answer: unknown = JSON.parse(greeted.stdout)
    const path = (answer as { inbox?: unknown }).inbox

    if (greeted.exitCode === 0 && typeof path === 'string') {
      bridge.inbox = path
      $.clock.every(POLL_MS, () => {
        void drain($, bridge)
      })
    }
  } catch {
    // Latch is unreachable: the session runs as it would without a bridge.
  }
}

export const register: Register = on => {
  const bridge: Bridge = {
    inbox: undefined,
    runningTurn: undefined,
    isDraining: false,
    questions: new Map(),
  }

  on('session.start', async ($, e, next) => {
    const session = await $.env.get('LATCH_SESSION_ID')

    if (session !== undefined && session !== '') {
      await open($, bridge)
    }

    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    bridge.runningTurn = e.turnId
    await report($, bridge, 'turn.start', { turn_id: e.turnId })

    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    // A subagent's run is a turn of its own loop; the conversation's turn is
    // the main loop's.
    if (e.agentId === undefined) {
      if (bridge.runningTurn === e.turnId) {
        bridge.runningTurn = undefined
      }

      await report($, bridge, 'turn.complete', {
        turn_id: e.turnId,
        reason: e.reason,
        duration_ms: e.durationMs,
      })
    }

    return next(e)
  })

  // The engine's own dialog still opens at the terminal. Whichever of the
  // terminal and Latch answers first settles the call; answering here ends
  // the dialog beneath, so the two can never both answer.
  on('tool.call', { tool: 'AskUserQuestion' }, async ($, e, next) => {
    const id = e.tool_use_id

    if (bridge.inbox === undefined || id === undefined) {
      return next(e)
    }

    const fromLatch = new Promise<Answers>(resolve => {
      bridge.questions.set(id, {
        asked: e.questions.map(question => question.question),
        answer: resolve,
      })
    })
    // The transcript gains the call only once it is answered, so this is how
    // the Hub learns a question is open, and under which id to answer it.
    await report($, bridge, 'question.open', {
      tool_use_id: id,
      questions: describeQuestions(e.questions),
    })

    const atTerminal = next(e).then(result => ({ result }))
    let answeredBy = 'nobody'

    try {
      const first = await Promise.race([
        atTerminal,
        fromLatch.then(answers => ({ answers })),
      ])

      if ('result' in first) {
        answeredBy = 'terminal'

        return first.result
      }

      answeredBy = 'latch'
      // The dialog beneath is abandoned when this hook returns.
      atTerminal.catch(() => undefined)

      return { result: { questions: e.questions, answers: first.answers } }
    } finally {
      bridge.questions.delete(id)
      await report($, bridge, 'question.closed', { tool_use_id: id, answered_by: answeredBy })
    }
  })

  on('session.end', async ($, e, next) => {
    await report($, bridge, 'session.end', { reason: e.reason })

    return next(e)
  })
}
