import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/work', surface: 'terminal', isInteractive: true } as const

test('an allow from Latch abandons the engine path and re-runs the call under an approving check', async ($, on) => {
  mock.env(on, { LATCH_PROBE_LOG: '/probe.log' })
  const clock = mock.clock(on)
  const checks: unknown[] = []
  const calls: { id: string | undefined; command: string }[] = []
  let releaseFirst: ((value: unknown) => void) | undefined

  on('fs.write', () => ({ value: undefined }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('tool.check', (_$, e) => {
    checks.push({ id: e.tool_use_id, input: e.input })
    return { decision: 'ask', reason: 'needs the person' }
  })
  on('tool.call', { tool: 'Bash' }, (_$, e) => {
    calls.push({ id: e.tool_use_id, command: e.command })
    if (calls.length === 1) {
      // The engine's dialog: open until the test says otherwise.
      return new Promise(resolve => {
        releaseFirst = resolve
      }) as never
    }
    return { result: { stdout: 'ran', stderr: '', interrupted: false } }
  })

  await $.session.start(START)
  const outcome = $.tool.call({ tool: 'Bash', command: 'touch x LATCH_PERM_PROBE_ALLOW' })
  await clock.advance(6000)
  const settled = await outcome

  expect(calls.length).toBe(2)
  expect(calls[0].command).toBe(calls[1].command)
  expect(calls[0].id).not.toBe(calls[1].id)
  expect(settled.deny).toBe(undefined)
  expect(settled.result).toEqual({ stdout: 'ran', stderr: '', interrupted: false })
  releaseFirst?.({ result: { stdout: 'late', stderr: '', interrupted: false } })
  // The kit's $.tool.call reaches the test's tool.call hook without raising
  // tool.check, so the one-shot approval is still held: the approving verdict
  // is what the real engine's permission check would have read.
  expect(checks).toEqual([])
  const approving = await $.tool.check({ tool: 'Bash', input: { command: 'touch x LATCH_PERM_PROBE_ALLOW' } })
  expect(approving).toEqual({ decision: 'allow', reason: 'approved in Latch' })
  const spent = await $.tool.check({ tool: 'Bash', input: { command: 'touch x LATCH_PERM_PROBE_ALLOW' } })
  expect(spent.decision).toBe('ask')
})

test('the check hook passes the engine verdict through and records the call id', async ($, on) => {
  mock.env(on, { LATCH_PROBE_LOG: '/probe.log' })
  mock.clock(on)
  const written: string[] = []

  on('fs.write', (_$, e) => {
    written.push(e.text)
    return { value: undefined }
  })
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('tool.check', () => ({ decision: 'ask', reason: 'needs the person', hook: 'PreToolUse' }))

  await $.session.start(START)
  const verdict = await $.tool.check({ tool: 'Bash', input: { command: 'rm -rf build' } })

  expect(verdict.decision).toBe('ask')
  expect(written.at(-1)).toContain('engine says ask')
})

test('a deny from Latch ends the call while the engine path is still open', async ($, on) => {
  mock.env(on, { LATCH_PROBE_LOG: '/probe.log' })
  const clock = mock.clock(on)
  const calls: string[] = []

  on('fs.write', () => ({ value: undefined }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('tool.check', () => ({ decision: 'ask' }))
  on('tool.call', { tool: 'Bash' }, (_$, e) => {
    calls.push(e.command)
    return new Promise(() => undefined) as never
  })

  await $.session.start(START)
  const outcome = $.tool.call({ tool: 'Bash', command: 'touch x LATCH_PERM_PROBE_DENY' })
  await clock.advance(6000)
  const settled = await outcome

  expect(calls).toEqual(['touch x LATCH_PERM_PROBE_DENY'])
  expect(settled.deny).toBe('Denied in Latch by the probe.')
})
