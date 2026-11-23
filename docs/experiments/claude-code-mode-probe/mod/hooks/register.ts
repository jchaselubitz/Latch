import type { Register } from 'claude-code'

type Host = Parameters<Parameters<Parameters<Register>[0]>[2]>[0]

let seq = 0
async function log($: Host, event: string, e: unknown): Promise<void> {
  const dir = (await $.env.get('MOD_LOG')) ?? '/tmp'
  const at = await $.clock.now()
  seq += 1
  await $.fs.write(`${dir}/${at}-${String(seq).padStart(4, '0')}.json`, JSON.stringify({ event, at, e }))
}

export const register: Register = on => {
  on('tool.call', async ($, e, next) => {
    await log($, 'tool.call', e)
    if ((await $.env.get('MOD_MODE')) === 'call-deny' && e.tool === 'Bash' && String((e as Record<string, unknown>).command).startsWith('touch')) {
      return { deny: 'The person denied this call in Latch.' }
    }
    const r = await next(e)
    await log($, 'tool.call.result', { tool: e.tool, tool_use_id: e.tool_use_id, r })
    return r
  })
  on('tool.check', async ($, e, next) => {
    await log($, 'tool.check', { e, origin: next.origin })
    const r = await next(e)
    await log($, 'tool.check.result', { tool: e.tool, r })
    return r
  })
  on('classic.PermissionRequest', async ($, e, next) => {
    await log($, 'classic.PermissionRequest', e)
    const mode = (await $.env.get('MOD_MODE')) ?? ''
    if (mode === 'deny-msg' && e.tool_name === 'Bash') {
      return { decision: { behavior: 'deny', message: 'The person denied this one call in Latch; carry on without it.' } }
    }
    if (mode === 'deny-empty' && e.tool_name === 'Bash') {
      return { decision: { behavior: 'deny' } }
    }
    if (mode === 'allow' && e.tool_name !== 'REPL') {
      return { decision: { behavior: 'allow' } }
    }
    return next(e)
  })
  on('classic.PreToolUse', async ($, e, next) => {
    await log($, 'classic.PreToolUse', e)
    return next(e)
  })
}
