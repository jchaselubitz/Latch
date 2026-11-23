import type { Register } from 'claude-code'

const LOG = '/tmp/latch-dialog-probe-mod.jsonl' // edit before a run

type Host = Parameters<Parameters<Parameters<Register>[0]>[2]>[0]

async function log($: Host, event: string, e: unknown): Promise<void> {
  const line = JSON.stringify({ event, e }) + '\n'
  let prior = ''
  try { prior = String(await $.fs.read(LOG)) } catch {}
  await $.fs.write(LOG, prior + line)
}

export const register: Register = on => {

  on('classic.PermissionRequest', async ($, e, next) => {
    await log($, 'classic.PermissionRequest', e)
    if (e.tool_name === 'ExitPlanMode') {
      return { decision: { behavior: 'allow', updatedInput: e.tool_input as Record<string, unknown>, updatedPermissions: [{ type: 'setMode', mode: 'acceptEdits', destination: 'session' }] } }
    }
    if (e.tool_name === 'Bash') {
      return { decision: { behavior: 'allow', updatedPermissions: (e.permission_suggestions ?? []).map(u => ({ ...u, destination: 'session' as const })) } }
    }
    return next(e)
  })

  on('classic.Elicitation', async ($, e, next) => {
    await log($, 'classic.Elicitation', e)
    const result = await next(e)
    await log($, 'classic.Elicitation.next', result)
    return result
  })

  on('classic.Notification', async ($, e, next) => {
    await log($, 'classic.Notification', e)
    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    await log($, 'tool.call', { tool: e.tool, tool_use_id: e.tool_use_id })
    return next(e)
  })
}
