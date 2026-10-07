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
  on('session.version', () => ({ value: { version: '2.1.292', builtAt: '' } }))
  on('session.model', () => ({ value: 'claude-test' }))
  on('command.list', () => ({
    value: [
      { name: 'compact', description: 'Compacts the conversation.', source: 'builtin' as const },
    ],
  }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('classic.PermissionRequest', () => ({}))
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
      bridge_version: 2,
      claude_version: '2.1.292',
      model: 'claude-test',
      commands: [{ name: 'compact', description: 'Compacts the conversation.', source: 'builtin' }],
    }),
    {
      bridge_event: 'turn.start',
      bridge_version: 2,
      timestamp: '1970-01-01T00:00:00.000Z',
      turn_id: 't1',
    },
    {
      bridge_event: 'turn.complete',
      bridge_version: 2,
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
    bridge_version: 2,
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

const BASH = { tool: 'Bash', command: 'touch marker', description: 'Create the marker file' } as const
const DIALOG = { tool_name: 'Bash', tool_input: { command: 'touch marker', description: 'Create the marker file' } }

test('a permission the person allows in Latch is re-run and the dialog is abandoned', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)
  const calls: { id: string | undefined; command: string }[] = []
  let isDialogAbandoned = false

  on('tool.check', () => ({ decision: 'ask', reason: 'needs the person' }))
  on('tool.call', { tool: 'Bash' }, (_$, e, next) => {
    calls.push({ id: e.tool_use_id, command: e.command })

    if (calls.length === 1) {
      // The engine's dialog: open until it is abandoned.
      return new Promise((_resolve, reject) => {
        next.signal.addEventListener('abort', () => {
          isDialogAbandoned = true
          reject(new Error('abandoned'))
        })
      }) as never
    }

    return { result: { stdout: 'ran', stderr: '', interrupted: false } }
  })

  await $.session.start(START)
  const call = $.tool.call({ ...BASH, tool_use_id: 'toolu_bash' })
  await clock.advance(1)
  await $.classic.PermissionRequest(DIALOG)

  queued.push(
    { id: 'cmd-1', kind: 'answer_permission', tool_use_id: 'toolu_other', decision: 'allow' },
    { id: 'cmd-2', kind: 'answer_permission', tool_use_id: 'toolu_bash', decision: 'maybe' },
    { id: 'cmd-3', kind: 'answer_permission', tool_use_id: 'toolu_bash', decision: 'allow' },
  )
  await clock.advance(500)

  expect((await call).result).toEqual({ stdout: 'ran', stderr: '', interrupted: false })
  expect(calls.map(made => made.command)).toEqual(['touch marker', 'touch marker'])
  expect(calls[0]?.id).not.toBe(calls[1]?.id)
  expect(isDialogAbandoned).toBe(true)
  // The approval is the re-run's alone: the model's own next identical call
  // is the engine's to decide.
  expect(await $.tool.check({ tool: 'Bash', input: { command: 'touch marker' } })).toEqual(
    expect.objectContaining({ decision: 'ask' }),
  )
  expect(latch.events().slice(1)).toEqual([
    expect.objectContaining({
      bridge_event: 'permission.open',
      tool_use_id: 'toolu_bash',
      tool: 'Bash',
      input: { command: 'touch marker', description: 'Create the marker file' },
    }),
    expect.objectContaining({ command_id: 'cmd-1', outcome: 'refused' }),
    expect.objectContaining({ command_id: 'cmd-2', outcome: 'refused' }),
    expect.objectContaining({ command_id: 'cmd-3', outcome: 'accepted' }),
    expect.objectContaining({
      bridge_event: 'permission.closed',
      tool_use_id: 'toolu_bash',
      answered_by: 'latch',
    }),
  ])
})

test('a deny from Latch ends the call while the dialog is still open', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)
  const calls: string[] = []

  on('tool.call', { tool: 'Bash' }, (_$, e, next) => {
    calls.push(e.command)

    return new Promise((_resolve, reject) => {
      next.signal.addEventListener('abort', () => reject(new Error('abandoned')))
    }) as never
  })

  await $.session.start(START)
  const call = $.tool.call({ ...BASH, tool_use_id: 'toolu_bash' })
  await clock.advance(1)
  await $.classic.PermissionRequest(DIALOG)
  queued.push({ id: 'cmd-1', kind: 'answer_permission', tool_use_id: 'toolu_bash', decision: 'deny' })
  await clock.advance(500)

  expect((await call).deny).toBe('The person denied this call in Latch.')
  expect(calls).toEqual(['touch marker'])
  expect(latch.events().slice(1)).toEqual([
    expect.objectContaining({ bridge_event: 'permission.open', tool_use_id: 'toolu_bash' }),
    expect.objectContaining({ command_id: 'cmd-1', outcome: 'accepted' }),
    expect.objectContaining({ bridge_event: 'permission.closed', answered_by: 'latch' }),
  ])
})

test('an answer at the terminal still settles the permission, and a late one is refused', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)
  let release: ((value: unknown) => void) | undefined

  on('tool.call', { tool: 'Bash' }, () =>
    new Promise(resolve => {
      release = resolve
    }) as never,
  )

  await $.session.start(START)
  const call = $.tool.call({ ...BASH, tool_use_id: 'toolu_bash' })
  await clock.advance(1)
  await $.classic.PermissionRequest(DIALOG)
  release?.({ result: { stdout: 'ran', stderr: '', interrupted: false } })

  expect((await call).result).toEqual({ stdout: 'ran', stderr: '', interrupted: false })
  expect(latch.events().at(-1)).toEqual(
    expect.objectContaining({ bridge_event: 'permission.closed', answered_by: 'terminal' }),
  )

  queued.push({ id: 'late', kind: 'answer_permission', tool_use_id: 'toolu_bash', decision: 'allow' })
  await clock.advance(500)
  expect(latch.events().at(-1)).toEqual(
    expect.objectContaining({ command_id: 'late', outcome: 'refused' }),
  )
})

test('a call that opens no dialog, and a dialog for no call of the model, announce nothing', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  mock.clock(on)
  const latch = latchHost(on)

  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: 'ran', stderr: '', interrupted: false } }))

  await $.session.start(START)
  await $.tool.call({ ...BASH, tool_use_id: 'toolu_bash' })
  await $.classic.PermissionRequest({ tool_name: 'Bash', tool_input: { command: 'rm -rf build' } })

  expect(latch.events().slice(1)).toEqual([])
})

test('a parallel batch announces every dialog under its own call id, answered in any order', async ($, on) => {
  mock.env(on, { LATCH_SESSION_ID: 'ses_1' })
  const clock = mock.clock(on)
  const queued: unknown[] = []
  const latch = latchHost(on, queued)
  const release = new Map<string, (value: unknown) => void>()

  // Each engine path stays open on its dialog until the terminal answers it
  // or the module abandons it.
  on('tool.call', { tool: 'Bash' }, (_$, e, next) =>
    new Promise((resolve, reject) => {
      release.set(e.command, resolve)
      next.signal.addEventListener('abort', () => reject(new Error('abandoned')))
    }) as never,
  )

  await $.session.start(START)
  const batch = ['a', 'b', 'c'].map(marker =>
    $.tool.call({
      tool: 'Bash',
      command: `touch ${marker}`,
      description: `Create marker ${marker}`,
      tool_use_id: `toolu_${marker}`,
    }),
  )
  await clock.advance(1)
  // The engine raises the hook for every queued dialog at once, while it
  // paints only the first.
  for (const marker of ['a', 'b', 'c']) {
    await $.classic.PermissionRequest({
      tool_name: 'Bash',
      tool_input: { command: `touch ${marker}`, description: `Create marker ${marker}` },
    })
  }

  // Latch denies the second, the terminal answers the first, Latch denies
  // the third.
  queued.push({ id: 'cmd-b', kind: 'answer_permission', tool_use_id: 'toolu_b', decision: 'deny' })
  await clock.advance(500)
  release.get('touch a')?.({ result: { stdout: 'ran', stderr: '', interrupted: false } })
  queued.push({ id: 'cmd-c', kind: 'answer_permission', tool_use_id: 'toolu_c', decision: 'deny' })
  await clock.advance(500)

  const [a, b, c] = await Promise.all(batch)
  expect(a?.result).toEqual({ stdout: 'ran', stderr: '', interrupted: false })
  expect(b?.deny).toBe('The person denied this call in Latch.')
  expect(c?.deny).toBe('The person denied this call in Latch.')
  expect(latch.events().slice(1)).toEqual([
    expect.objectContaining({
      bridge_event: 'permission.open',
      tool_use_id: 'toolu_a',
      input: { command: 'touch a', description: 'Create marker a' },
    }),
    expect.objectContaining({
      bridge_event: 'permission.open',
      tool_use_id: 'toolu_b',
      input: { command: 'touch b', description: 'Create marker b' },
    }),
    expect.objectContaining({
      bridge_event: 'permission.open',
      tool_use_id: 'toolu_c',
      input: { command: 'touch c', description: 'Create marker c' },
    }),
    expect.objectContaining({ command_id: 'cmd-b', outcome: 'accepted' }),
    expect.objectContaining({ bridge_event: 'permission.closed', tool_use_id: 'toolu_b', answered_by: 'latch' }),
    expect.objectContaining({ bridge_event: 'permission.closed', tool_use_id: 'toolu_a', answered_by: 'terminal' }),
    expect.objectContaining({ command_id: 'cmd-c', outcome: 'accepted' }),
    expect.objectContaining({ bridge_event: 'permission.closed', tool_use_id: 'toolu_c', answered_by: 'latch' }),
  ])
})
