// Experiment for docs/CLAUDE_BRIDGE.md: can a hooks module answer a
// permission prompt by call id?
//
// tool.check reports the engine's verdict for each Bash call. tool.call
// races the engine's own path (dialog, then tool) against a simulated Latch
// answer that arrives after ANSWER_AFTER_MS. ALLOW abandons the dialog and
// re-runs the call through $.tool.call under an approving tool.check; DENY
// ends the call with a denial while the dialog is open.

import type { Register } from 'claude-code'

const ALLOW = 'LATCH_PERM_PROBE_ALLOW'
const DENY = 'LATCH_PERM_PROBE_DENY'
const ANSWER_AFTER_MS = 6000

type Host = Parameters<Parameters<Parameters<Register>[0]>[2]>[0]

let log = ''

async function note($: Host, line: string) {
  log += `${new Date().toISOString()} ${line}\n`
  try {
    const path = (await $.env.get('LATCH_PROBE_LOG')) ?? '/tmp/latch-perm-probe.log'
    await $.fs.write(path, log)
  } catch {
    // logging is best effort
  }
}

export const register: Register = on => {
  // One-shot approvals keyed by the Bash command, granted "in Latch".
  const approved = new Set<string>()

  on('tool.check', { tool: 'Bash' }, async ($, e, next) => {
    const input = e.input as { command?: string }
    const command = input.command ?? ''
    if (approved.has(command)) {
      approved.delete(command)
      await note($, `check ${e.tool_use_id ?? '(query)'} origin=${next.origin.plugin}/${next.origin.tier}: ALLOW by probe`)
      return { decision: 'allow', reason: 'approved in Latch' }
    }
    const verdict = await next(e)
    await note($, `check ${e.tool_use_id ?? '(query)'} origin=${next.origin.plugin}/${next.origin.tier}: engine says ${verdict.decision} (${verdict.reason ?? ''} rule=${verdict.rule ?? ''} hook=${verdict.hook ?? ''})`)
    return verdict
  }).catch(async ($, e, next) => {
    await note($, `check catch: ${next.error?.kind ?? ''} ${String(next.error?.message ?? next.error)}`)
    return next(e)
  })

  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const wantsAllow = e.command.includes(ALLOW)
    const wantsDeny = e.command.includes(DENY)
    if (!wantsAllow && !wantsDeny) {
      return next(e)
    }
    await note($, `call ${e.tool_use_id} opened: ${e.command}`)
    const atTerminal = next(e).then(result => ({ result }))
    atTerminal.then(
      ({ result }) => note($, `call ${e.tool_use_id} engine path settled: ${JSON.stringify(result).slice(0, 300)}`),
      error => note($, `call ${e.tool_use_id} engine path rejected: ${String(error)}`),
    )
    const fromLatch = $.clock.sleep(ANSWER_AFTER_MS).then(() => ({ latch: wantsAllow ? 'allow' : 'deny' }))
    const first = await Promise.race([atTerminal, fromLatch])
    if ('result' in first) {
      await note($, `call ${e.tool_use_id}: terminal answered first`)
      return first.result
    }
    if (first.latch === 'deny') {
      await note($, `call ${e.tool_use_id}: Latch denies; returning deny with the dialog open`)
      return { deny: 'Denied in Latch by the probe.' }
    }
    await note($, `call ${e.tool_use_id}: Latch allows; abandoning the dialog and re-calling`)
    approved.add(e.command)
    try {
      const again = await $.tool.call({
        tool: 'Bash',
        command: e.command,
        description: e.description,
        consent: 'The user pressed "Allow" in Latch.',
      })
      await note($, `call ${e.tool_use_id}: re-call settled: ${JSON.stringify(again).slice(0, 400)}`)
      if (again.deny !== undefined) {
        return { deny: again.deny }
      }
      if (again.isError === true) {
        return { deny: `The re-run failed: ${again.text ?? ''}` }
      }
      return { result: again.result }
    } catch (error) {
      await note($, `call ${e.tool_use_id}: re-call rejected: ${String(error)}`)
      return { deny: `The re-run was rejected: ${String(error)}` }
    }
  }).catch(async ($, e, next) => {
    await note($, `call catch: ${next.error?.kind ?? ''} ${String(next.error?.message ?? next.error)} called=${next.called}`)
    return next.called ? next(e) : { deny: 'probe guard failed' }
  })
}
