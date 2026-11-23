// Latch's bridge into a Claude Code session it hosts.
//
// The Conversation Hub observes Claude from the outside: it tails the
// transcript and reads the terminal. This module is the inside half. It tells
// the Hub what only the engine knows (a turn began, a turn ended and why,
// which commands the session accepts, which call is waiting on a permission)
// and carries out what the Hub asks of the session (submit this prompt, stop
// this turn, answer this question, allow or deny this call) through the
// engine's own calls instead of keystrokes.
//
// A hooks module has no file or socket API of its own, so every exchange with
// Latch is one run of the `latch` binary that launched the session:
//
//   latch __conversation-bridge hello   record on stdin, answers { inbox }
//   latch __conversation-bridge event   record on stdin, appended to the sidecar
//   latch __conversation-bridge take    answers the queued commands, once each
//
// Outside a Latch session (no LATCH_SESSION_ID) the module does nothing.

import type { PermissionRequestDecision, Register, ToolCallArgs } from 'claude-code'

import { BRIDGE_VERSION, LATCH_BIN } from './config'

/** How often the inbox directory is looked at. A look is one directory read. */
const POLL_MS = 500
/** How long a submitted prompt may take to enter before it is reported queued. */
const SUBMIT_SETTLE_MS = 2000
const RUN_TIMEOUT_MS = 5000
const MAX_COMMANDS = 200
const MAX_DESCRIPTION = 160
const MAX_QUESTION_TEXT = 1000
/** The name in `.claude-plugin/plugin.json`: what `next.origin` calls this module. */
const PLUGIN_NAME = 'latch-conversation-bridge'
/** How much of each string argument a `permission.open` record carries. */
const MAX_INPUT_TEXT = 1000
const MAX_INPUT_FIELDS = 32
/**
 * How long the approval for a re-run outlives the re-run's own permission check.
 * Claude Code 2.1.321 stopped raising a permission prompt for a module's own
 * `$.tool.call`, which makes this approval redundant there. Latch hosts
 * whichever Claude is installed and sets no minimum version (the bridge was
 * built against 2.1.292), so the approval stays until a minimum of 2.1.321 or
 * later is enforced.
 */
const APPROVAL_TTL_MS = 30_000
const CONSENT = 'The person pressed "Allow" in Latch.'
const DENIED = 'The person denied this call in Latch.'

type Command = {
  id?: unknown
  kind?: unknown
  text?: unknown
  tool_use_id?: unknown
  answers?: unknown
  decision?: unknown
  option?: unknown
}
type Decision = 'allow' | 'deny'
/** A permission update the engine suggested ("don't ask again for ..."). */
type Allowed = Extract<PermissionRequestDecision, { behavior: 'allow' }>
type Suggestion = NonNullable<Allowed['updatedPermissions']>[number]
type SetMode = Extract<Suggestion, { type: 'setMode' }>
/** An answer only the engine's own permission hook can carry out. */
type HookAnswer = { option: string; text?: string }
/** What the hook waits on while the dialog is open. */
type HookSlot = { answer: (answer: HookAnswer) => void; suggestions: Suggestion[] }
/** A tool call of the model's whose permission dialog Latch may answer. */
type OpenPermission = {
  tool: string
  /** The tool's arguments, as the dialog decides on them. */
  input: Record<string, unknown>
  /** True once the engine raised the dialog and the Hub was told. */
  isAnnounced: boolean
  answer: (decision: Decision) => void
  /** Set once the engine's permission hook is waiting for the phone. */
  hook?: HookSlot
  /** The first answer Latch gave, by either path. A second is refused. */
  latch?: string
  /** Lets the permission hook go when the call ends another way. */
  release?: () => void
  /** True for a call a Code Mode script made: only its hook settles it. */
  inScript: boolean
}
/** The words of each option the permission hook can carry out. */
type OfferedOption = { id: string; label: string; requires_text: boolean }
const PLAN_MODES: Record<string, string> = {
  approve_plan: 'default',
  approve_plan_accept_edits: 'acceptEdits',
}
const MAX_PLAN = 8192
const MAX_LABEL = 200
/** The computer-use tool whose dialog is raised inside the call itself. */
const APP_ACCESS_TOOL = 'mcp__computer-use__request_access'
/**
 * Whether a call was made inside a Code Mode script rather than by the model
 * directly. The engine names script calls this way (`repl_` or `toolu_cc_`),
 * and the transcript files them under the REPL call that ran them.
 */
const isScriptCall = (id: string): boolean => id.startsWith('repl_') || id.startsWith('toolu_cc_')
/** A dialog this short-lived never reaches the phone. */
const TERMINAL_DIALOG_DELAY_MS = 1500
/** A re-run of a call the person allowed, waiting on its own permission check. */
type Approval = { tool: string; until: number }
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
  /** The model's tool calls still running, by tool_use_id, with their dialog's answer. */
  permissions: Map<string, OpenPermission>
  /** One-shot approvals for this module's own re-runs. */
  approvals: Approval[]
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

/** The tool's own arguments: the call's envelope without the engine's keys. */
function toolArguments(call: Record<string, unknown>): Record<string, unknown> {
  const { tool: _tool, tool_use_id: _id, agentId: _loop, ...rest } = call

  return rest
}

/** One spelling for equal values, so a dialog can be matched to its call. */
function canonical(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonical).join(',')}]`
  }

  if (typeof value === 'object' && value !== null) {
    const record = value as Record<string, unknown>

    return `{${Object.keys(record)
      .sort()
      .map(key => `${JSON.stringify(key)}:${canonical(record[key])}`)
      .join(',')}}`
  }

  return JSON.stringify(value) ?? 'undefined'
}

/** The arguments as the Hub may show them: named scalars, each bounded. */
function describeInput(input: Record<string, unknown>): Record<string, unknown> {
  const described: Record<string, unknown> = {}

  for (const [key, value] of Object.entries(input).slice(0, MAX_INPUT_FIELDS)) {
    if (typeof value === 'string') {
      described[key] = value.slice(0, MAX_INPUT_TEXT)
    } else if (typeof value === 'number' || typeof value === 'boolean') {
      described[key] = value
    }
  }

  return described
}

/**
 * Answers an open permission dialog for the person. The decision is aimed at
 * one call by its id, and only at one the Hub was told about.
 */
function answerPermission(bridge: Bridge, command: Command): Outcome {
  const open =
    typeof command.tool_use_id === 'string' ? bridge.permissions.get(command.tool_use_id) : undefined

  if (open === undefined || !open.isAnnounced) {
    return { outcome: 'refused', detail: 'the permission request is no longer open' }
  }

  if (open.latch !== undefined) {
    return { outcome: 'refused', detail: 'the permission request was already answered' }
  }

  if (typeof command.option === 'string') {
    const text = typeof command.text === 'string' ? command.text.trim().slice(0, 4096) : undefined

    if (open.hook === undefined || !offeredIds(open).includes(command.option)) {
      return { outcome: 'refused', detail: 'that option is not offered for this request' }
    }

    if (needsText(command.option) && (text === undefined || text === '')) {
      return { outcome: 'refused', detail: 'this option needs a message' }
    }

    open.latch = command.option
    open.hook.answer({ option: command.option, text })

    return { outcome: 'accepted' }
  }

  if (command.decision !== 'allow' && command.decision !== 'deny') {
    return { outcome: 'refused', detail: 'the decision must be allow or deny' }
  }

  // ExitPlanMode is settled only by its permission hook: running it again
  // would open a second dialog.
  if (open.tool === 'ExitPlanMode') {
    return { outcome: 'refused', detail: 'a plan is approved or rejected, not allowed' }
  }

  // A call inside a Code Mode script is settled by its permission hook too.
  // Ending it from `tool.call` would hand the script a refusal it did not
  // ask for, and a re-run would run outside the script. A deny carries a
  // message, so the engine refuses this one call and the script goes on.
  if (open.inScript) {
    if (open.hook === undefined) {
      return { outcome: 'refused', detail: 'the permission request is not open yet' }
    }

    open.latch = command.decision
    open.hook.answer({ option: command.decision === 'allow' ? 'allow_once' : 'deny' })

    return { outcome: 'accepted' }
  }

  open.latch = command.decision
  open.answer(command.decision)

  return { outcome: 'accepted' }
}

function needsText(option: string): boolean {
  return option === 'deny_with_feedback' || option === 'reject_plan_with_feedback'
}

function offeredIds(open: OpenPermission): string[] {
  return offeredOptions(open.tool, open.hook?.suggestions ?? []).map(option => option.id)
}

const isRuleUpdate = (update: Suggestion): boolean =>
  update.type === 'addRules' || update.type === 'addDirectories'

/** A short account of what a suggestion would stop asking about. */
function describeSuggestion(update: Suggestion): string {
  const fields = update as unknown as Record<string, unknown>
  const rules = Array.isArray(fields.rules) ? (fields.rules as Record<string, unknown>[]) : []
  const parts = rules
    .map(rule => `${String(rule.toolName ?? '')}${rule.ruleContent ? `(${String(rule.ruleContent)})` : ''}`)
    .filter(part => part !== '')
  const dirs = Array.isArray(fields.directories) ? (fields.directories as unknown[]).map(String) : []

  return [...parts, ...dirs].join(', ').slice(0, MAX_LABEL)
}

/** The options the permission hook can carry out for this dialog. */
function offeredOptions(tool: string, suggestions: readonly Suggestion[]): OfferedOption[] {
  if (tool === 'ExitPlanMode') {
    return [
      { id: 'approve_plan', label: 'Yes, and manually approve edits', requires_text: false },
      { id: 'approve_plan_accept_edits', label: 'Yes, and auto-accept edits', requires_text: false },
      { id: 'reject_plan_with_feedback', label: 'No, keep planning', requires_text: true },
    ]
  }

  const offered: OfferedOption[] = []
  const rules = suggestions.filter(isRuleUpdate)

  if (rules.length > 0) {
    offered.push({ id: 'allow_session', label: 'Yes, for this session', requires_text: false })

    if (rules.some(update => (update as { destination?: string }).destination !== 'session')) {
      const what = rules.map(describeSuggestion).filter(text => text !== '').join('; ')

      offered.push({
        id: 'allow_always',
        label: `Yes, and don't ask again${what === '' ? '' : ` for ${what}`}`.slice(0, MAX_LABEL),
        requires_text: false,
      })
    }
  }

  for (const update of suggestions) {
    if (update.type === 'setMode' && /^[A-Za-z0-9]{1,32}$/.test(update.mode)) {
      offered.push({ id: `allow_mode_${update.mode}`, label: `Yes, and switch to ${update.mode}`, requires_text: false })
    }
  }

  offered.push({ id: 'deny_with_feedback', label: 'No, and tell Claude what to do instead', requires_text: true })

  return offered
}

/** The engine's own verdict for an answer given in Latch. */
function verdict(
  open: OpenPermission,
  answer: HookAnswer,
  input: Record<string, unknown>,
): PermissionRequestDecision {
  const suggestions = open.hook?.suggestions ?? []
  const message = answer.text ?? DENIED

  if (answer.option in PLAN_MODES) {
    const setMode: SetMode = {
      type: 'setMode',
      mode: PLAN_MODES[answer.option] as SetMode['mode'],
      destination: 'session',
    }

    // A tool that waits on a person is settled only with its input.
    return { behavior: 'allow', updatedInput: input, updatedPermissions: [setMode] }
  }

  if (
    answer.option === 'reject_plan_with_feedback' ||
    answer.option === 'deny_with_feedback' ||
    answer.option === 'deny'
  ) {
    return { behavior: 'deny', message }
  }

  if (answer.option === 'allow_once') {
    return { behavior: 'allow' }
  }

  if (answer.option === 'allow_session') {
    return {
      behavior: 'allow',
      updatedPermissions: suggestions
        .filter(isRuleUpdate)
        .map(update => ({ ...update, destination: 'session' }) as Suggestion),
    }
  }

  if (answer.option === 'allow_always') {
    return { behavior: 'allow', updatedPermissions: suggestions.filter(isRuleUpdate) }
  }

  const mode = answer.option.startsWith('allow_mode_') ? answer.option.slice('allow_mode_'.length) : undefined

  return {
    behavior: 'allow',
    updatedPermissions: suggestions.filter(update => update.type === 'setMode' && update.mode === mode),
  }
}

/**
 * Runs a call the person allowed in Latch once more, under this module's own
 * one-shot approval, and hands its result back as the original call's. The
 * engine's own path for the original call, dialog included, is abandoned
 * when the hook that called this returns.
 */
async function rerun(
  $: Host,
  bridge: Bridge,
  tool: string,
  input: Record<string, unknown>,
): Promise<{ deny: string } | { result: unknown } | { result: unknown; text?: string; isError: true }> {
  const approval: Approval = { tool, until: (await $.clock.now()) + APPROVAL_TTL_MS }
  bridge.approvals.push(approval)

  try {
    const again = await $.tool.call({ ...input, tool, consent: CONSENT } as ToolCallArgs)

    if (again.deny !== undefined) {
      return { deny: again.deny }
    }

    if (again.isError === true) {
      return { result: again.result, text: again.text, isError: true }
    }

    return { result: again.result }
  } catch (error) {
    return { deny: `Latch allowed this call, but it could not be run again: ${reason(error)}` }
  } finally {
    bridge.approvals = bridge.approvals.filter(held => held !== approval)
  }
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

  if (command.kind === 'answer_permission') {
    return answerPermission(bridge, command)
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
      'answer_permission',
      'permission_options',
      'terminal_dialogs',
      'command_catalog',
      'session_usage',
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

/** How many rate-limit windows a usage report carries. */
const MAX_RATE_LIMITS = 8

/**
 * Reports the session's model, context window, rate limits and cost: the
 * status line's figures. Read once per main-loop turn, so it costs nothing
 * while the session is idle.
 */
async function reportUsage($: Host, bridge: Bridge): Promise<void> {
  if (bridge.inbox === undefined) {
    return
  }

  try {
    const usage = await $.session.usage()
    await report($, bridge, 'session.usage', {
      model: await $.session.model(),
      context_tokens: usage.context.tokens,
      context_window: usage.context.window,
      context_percent: usage.context.percent,
      rate_limits: usage.rateLimits.slice(0, MAX_RATE_LIMITS).map(limit => ({
        kind: limit.kind,
        percent_used: limit.percentUsed,
        resets_at: limit.resetsAt,
      })),
      cost_usd: usage.cost?.usd,
    })
  } catch {
    // An engine without these figures leaves the phone without them.
  }
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
    permissions: new Map(),
    approvals: [],
  }

  on('session.start', async ($, e, next) => {
    const session = await $.env.get('LATCH_SESSION_ID')

    if (session !== undefined && session !== '') {
      await open($, bridge)
      await reportUsage($, bridge)
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
      await reportUsage($, bridge)
    }

    return next(e)
  })

  // A permission dialog is the engine's, and a hook cannot settle it as
  // allowed. What a hook can do is end the call beneath it: a deny is
  // returned with the dialog open, and an allow abandons the engine's path and
  // runs the call again under the one-shot approval below. Whichever of the
  // terminal and Latch answers first settles the call.
  on('tool.call', async ($, e, next) => {
    const id = e.tool_use_id

    if (bridge.inbox === undefined || e.tool === 'AskUserQuestion' || id === undefined) {
      return next(e)
    }

    // A dialog raised inside a tool call (computer-use app access) has no hook
    // that could answer it. It is reported, only if it outlasts a moment, so the
    // phone can say where to answer; the call itself runs as the engine would.
    if (e.tool === APP_ACCESS_TOOL) {
      const raw = e as unknown as Record<string, unknown>
      const running = next(e)
      const ended = running.then(
        () => true,
        () => true,
      )
      const quick = await Promise.race([ended, $.clock.sleep(TERMINAL_DIALOG_DELAY_MS).then(() => false)])

      if (!quick) {
        await report($, bridge, 'terminal.open', {
          tool_use_id: id,
          kind: 'app_access',
          apps: Array.isArray(raw.apps) ? raw.apps.slice(0, 8).map(app => String(app).slice(0, 64)) : [],
          request_reason: typeof raw.reason === 'string' ? raw.reason.slice(0, MAX_INPUT_TEXT) : undefined,
        })
      }

      try {
        return await running
      } finally {
        if (!quick) {
          await report($, bridge, 'terminal.closed', { tool_use_id: id, answered_by: 'terminal' })
        }
      }
    }

    const fromLatch = new Promise<Decision>(resolve => {
      bridge.permissions.set(id, {
        tool: e.tool,
        input: toolArguments(e as Record<string, unknown>),
        isAnnounced: false,
        // A plan, and a call inside a script, are settled by the permission
        // hook alone.
        answer: e.tool === 'ExitPlanMode' || isScriptCall(id) ? () => undefined : resolve,
        inScript: isScriptCall(id),
      })
    })
    const atTerminal = next(e).then(result => ({ result }))
    let answeredBy = 'nobody'
    let decision: string | undefined

    try {
      const first = await Promise.race([
        atTerminal,
        fromLatch.then(decision => ({ decision })),
      ])

      if ('result' in first) {
        // An answer given in Latch through the permission hook ends the call
        // the engine's way: the call's own result arrives here.
        const given = bridge.permissions.get(id)?.latch
        answeredBy = given === undefined ? 'terminal' : 'latch'
        decision = given

        return first.result
      }

      answeredBy = 'latch'
      decision = first.decision
      // The dialog beneath is abandoned when this hook returns.
      atTerminal.catch(() => undefined)

      if (first.decision === 'deny') {
        return { deny: DENIED }
      }

      return rerun($, bridge, e.tool, toolArguments(e as Record<string, unknown>))
    } finally {
      const open = bridge.permissions.get(id)
      bridge.permissions.delete(id)
      open?.release?.()

      if (open?.isAnnounced === true) {
        await report($, bridge, 'permission.closed', { tool_use_id: id, answered_by: answeredBy, decision })
      }
    }
  }).catch(($, e, next) => next(e))

  // The engine raises this as it opens a permission dialog, and only then:
  // a call the mode or a rule settles never gets here. It carries no call id,
  // so the dialog is matched to the running call with the same arguments.
  on('classic.PermissionRequest', async ($, e, next) => {
    const waiting = [...bridge.permissions].filter(
      ([, open]) => !open.isAnnounced && open.tool === e.tool_name,
    )
    const asked = canonical(e.tool_input)
    const match =
      waiting.find(([, open]) => canonical(open.input) === asked) ??
      (waiting.length === 1 ? waiting[0] : undefined)

    if (match !== undefined) {
      const [id, open] = match
      open.isAnnounced = true
      const suggestions = (Array.isArray(e.permission_suggestions)
        ? (e.permission_suggestions as unknown as Suggestion[])
        : []
      ).slice(0, 8)
      const toolInput = e.tool_input as Record<string, unknown>
      const answered = new Promise<HookAnswer>(resolve => {
        open.hook = { answer: resolve, suggestions }
      })
      const released = new Promise<undefined>(resolve => {
        open.release = () => resolve(undefined)
      })
      const plan = open.tool === 'ExitPlanMode'

      await report($, bridge, 'permission.open', {
        tool_use_id: id,
        tool: open.tool,
        cwd: 'cwd' in e && typeof e.cwd === 'string' ? e.cwd.slice(0, 1024) : undefined,
        input: plan ? {} : describeInput(open.input),
        plan: plan && typeof toolInput.plan === 'string' ? toolInput.plan.slice(0, MAX_PLAN) : undefined,
        plan_path:
          plan && typeof toolInput.planFilePath === 'string' ? toolInput.planFilePath.slice(0, 1024) : undefined,
        options: offeredOptions(open.tool, suggestions),
        in_script: open.inScript ? true : undefined,
      })

      // The engine runs this hook beside the dialog it paints, and whichever
      // answers first settles the call. Hold the hook for the phone, and let
      // it go when the call ends some other way.
      const first = await Promise.race([answered, released])

      if (first !== undefined) {
        return { decision: verdict(open, first, toolInput) }
      }
    }

    return next(e)
  })

  // The approval is read only by this module's own re-run of a call the
  // person allowed: the model's next identical call is the engine's to decide.
  on('tool.check', async ($, e, next) => {
    if (next.origin.plugin === PLUGIN_NAME) {
      const now = await $.clock.now()
      bridge.approvals = bridge.approvals.filter(held => held.until > now)
      const index = bridge.approvals.findIndex(held => held.tool === e.tool)

      if (index !== -1) {
        bridge.approvals.splice(index, 1)

        return { decision: 'allow', reason: CONSENT }
      }
    }

    return next(e)
  }).catch(($, e, next) => next(e))

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
      await report($, bridge, 'question.closed', {
        tool_use_id: id,
        answered_by: answeredBy,
        decision: answeredBy === 'nobody' ? undefined : 'submitted',
      })
    }
  })

  on('session.end', async ($, e, next) => {
    await report($, bridge, 'session.end', { reason: e.reason })

    return next(e)
  })
}
