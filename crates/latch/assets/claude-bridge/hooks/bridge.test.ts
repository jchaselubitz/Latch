import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

type Run = { action: string; record: Record<string, unknown> | undefined }

/**
 * Stands for Latch beneath the module: answers its `latch` runs from memory
 * and keeps what each carried. `queued` is what the next `take` hands over.
 */
function latchHost(on: On, queued: unknown[] = []) {
  const runs: Run[] = []

  on('process.run', (_$, e) => {
    const action = String(e.argv[2])
    const stdin = e.init?.stdin ?? ''
    runs.push({ action, record: stdin === '' ? undefined : JSON.parse(stdin) })

    const stdout =
      action === 'take' ? JSON.stringify(queued.splice(0)) : JSON.stringify({ inbox: '/inbox' })

    return {
      value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
    }
  })
  on('fs.list', () => ({
    value: queued.map((_, index) => ({
      name: `${index}.json`,
      kind: 'file' as const,
      size: 1,
      mtimeMs: 0,
      isLink: false,
    })),
  }))
  on('session.version', () => ({ value: { version: '2.1.288', builtAt: '' } }))
  on('session.model', () => ({ value: 'claude-test' }))
  on('command.list', () => ({
    value: [
      { name: 'compact', description: 'Compacts the conversation.', source: 'builtin' as const },
    ],
  }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('turn.start', (_$, e) => ({ turnId: e.turnId }))
  on('turn.complete', (_$, e) => ({ text: e.answer }))

  const events = () => runs.filter(run => run.action !== 'take').map(run => run.record)

  return { runs, events }
}

const START = { cwd: '/work', surface: 'terminal', isInteractive: true } as const
const DONE = { answer: 'ok', durationMs: 1200, isAborted: false, reason: 'answer' } as const

test('outside a Latch session the bridge runs nothing', async ($, on) => {
  mock.env(on, {})
  const latch = latchHost(on)

  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })
  await $.turn.complete({ ...DONE, turnId: 't1' })

  expect(latch.runs).toEqual([])
})

test('it greets Latch and reports the main loop turn boundaries', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  mock.clock(on)
  const latch = latchHost(on)

  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })
  await $.turn.complete({ ...DONE, turnId: 'sub', agentId: 'agent-1' })
  await $.turn.complete({ ...DONE, turnId: 't1', reason: 'aborted', isAborted: true })

  expect(latch.events()).toEqual([
    expect.objectContaining({
      bridge_version: 1,
      claude_version: '2.1.288',
      model: 'claude-test',
      commands: [{ name: 'compact', description: 'Compacts the conversation.', source: 'builtin' }],
    }),
    {
      bridge_event: 'turn.start',
      bridge_version: 1,
      timestamp: '1970-01-01T00:00:00.000Z',
      turn_id: 't1',
    },
    {
      bridge_event: 'turn.complete',
      bridge_version: 1,
      timestamp: '1970-01-01T00:00:00.000Z',
      turn_id: 't1',
      reason: 'aborted',
      duration_ms: 1200,
    },
  ])
})

test('a queued prompt is submitted as the person and acknowledged', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const latch = latchHost(on, [{ id: 'cmd-1', kind: 'submit_prompt', text: 'run the tests' }])
  const entered: unknown[] = []

  on('prompt.submit', (_$, e) => {
    entered.push({ text: e.text, origin: e.origin })

    return { text: e.text, origin: e.origin }
  })

  await $.session.start(START)
  await clock.advance(500)

  // Submitted as the person's own words, so the model reads it unframed.
  expect(entered).toEqual([
    {
      text: 'run the tests',
      origin: { kind: 'plugin', name: 'latch-conversation-bridge', asUser: true },
    },
  ])
  expect(latch.events().at(-1)).toEqual({
    bridge_event: 'command.result',
    bridge_version: 1,
    timestamp: '1970-01-01T00:00:00.500Z',
    command_id: 'cmd-1',
    outcome: 'accepted',
  })
})

test('stopping with no running turn is refused, and a running one is aborted', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = [{ id: 'cmd-1', kind: 'abort_turn' }]
  const latch = latchHost(on, queued)
  const aborted: string[] = []

  on('turn.abort', (_$, e) => {
    aborted.push(e.turnId)

    return { value: undefined }
  })

  await $.session.start(START)
  await clock.advance(500)
  expect(latch.events().at(-1)).toEqual(
    expect.objectContaining({ command_id: 'cmd-1', outcome: 'refused', detail: 'no turn is running' }),
  )

  await $.turn.start({ text: 'hi', turnId: 't7' })
  queued.push({ id: 'cmd-2', kind: 'abort_turn' }, { id: 'cmd-3', kind: 'reticulate' })
  await clock.advance(500)

  expect(aborted).toEqual(['t7'])
  expect(latch.events().slice(-2)).toEqual([
    expect.objectContaining({ command_id: 'cmd-2', outcome: 'accepted' }),
    expect.objectContaining({ command_id: 'cmd-3', outcome: 'refused' }),
  ])
})

const QUESTION = {
  question: 'Which color?',
  header: 'Color',
  options: [
    { label: 'Red', description: 'Warm.' },
    { label: 'Blue', description: 'Cool.' },
  ],
  multiSelect: false,
}

test('an answer from Latch settles the open question and ends the dialog', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)
  let isDialogAbandoned = false

  // The terminal's dialog: open until someone answers, here nobody does.
  on('tool.call', { tool: 'AskUserQuestion' }, (_$, _e, next) =>
    new Promise((_resolve, reject) => {
      next.signal.addEventListener('abort', () => {
        isDialogAbandoned = true
        reject(new Error('abandoned'))
      })
    }),
  )

  await $.session.start(START)
  const call = $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu_q', questions: [QUESTION] })

  queued.push(
    { id: 'cmd-1', kind: 'answer_question', tool_use_id: 'toolu_other', answers: { 'Which color?': 'Red' } },
    { id: 'cmd-2', kind: 'answer_question', tool_use_id: 'toolu_q', answers: { 'Which size?': 'Big' } },
    { id: 'cmd-3', kind: 'answer_question', tool_use_id: 'toolu_q', answers: { 'Which color?': 'Chartreuse' } },
  )
  await clock.advance(500)

  expect((await call).result).toEqual(
    expect.objectContaining({ answers: { 'Which color?': 'Chartreuse' } }),
  )
  expect(isDialogAbandoned).toBe(true)
  expect(latch.events().slice(1)).toEqual([
    expect.objectContaining({
      bridge_event: 'question.open',
      tool_use_id: 'toolu_q',
      questions: [
        {
          question: 'Which color?',
          header: 'Color',
          multi_select: false,
          options: [
            { label: 'Red', description: 'Warm.' },
            { label: 'Blue', description: 'Cool.' },
          ],
        },
      ],
    }),
    expect.objectContaining({ command_id: 'cmd-1', outcome: 'refused', detail: 'the question is no longer open' }),
    expect.objectContaining({ command_id: 'cmd-2', outcome: 'refused' }),
    expect.objectContaining({ command_id: 'cmd-3', outcome: 'accepted' }),
    expect.objectContaining({ bridge_event: 'question.closed', tool_use_id: 'toolu_q', answered_by: 'latch' }),
  ])
})

test('an answer at the terminal still settles the question', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)

  on('tool.call', { tool: 'AskUserQuestion' }, (_$, e) => ({
    result: { questions: e.questions, answers: { 'Which color?': 'Blue' } },
  }))

  await $.session.start(START)
  const answered = await $.tool.call({
    tool: 'AskUserQuestion',
    tool_use_id: 'toolu_q',
    questions: [QUESTION],
  })

  expect(answered.result).toEqual(expect.objectContaining({ answers: { 'Which color?': 'Blue' } }))
  expect(latch.events().at(-1)).toEqual(
    expect.objectContaining({ bridge_event: 'question.closed', answered_by: 'terminal' }),
  )

  queued.push({ id: 'late', kind: 'answer_question', tool_use_id: 'toolu_q', answers: { 'Which color?': 'Red' } })
  await clock.advance(500)
  expect(latch.events().at(-1)).toEqual(
    expect.objectContaining({ command_id: 'late', outcome: 'refused' }),
  )
})
