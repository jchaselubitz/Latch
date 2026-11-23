// Minimal MCP stdio server that elicits (form and url). No dependencies.
import { appendFileSync } from 'node:fs'
import { createInterface } from 'node:readline'
const LOG = process.env.ELICIT_LOG
const log = (o) => LOG && appendFileSync(LOG, JSON.stringify(o) + '\n')
let nextId = 1000
const waiting = new Map()
const send = (o) => { log({ out: o }); process.stdout.write(JSON.stringify(o) + '\n') }
const ask = (method, params) => new Promise(r => { const id = nextId++; waiting.set(id, r); send({ jsonrpc: '2.0', id, method, params }) })
const tools = [
  { name: 'ask_form', description: 'Ask the user for a display name, a count and a colour through an MCP form elicitation.', inputSchema: { type: 'object', properties: {} } },
  { name: 'approve', description: 'Permission prompt tool for the Latch probe.', inputSchema: { type: 'object', properties: { tool_name: { type: 'string' }, input: { type: 'object' }, tool_use_id: { type: 'string' } } } },
  { name: 'ask_url', description: 'Ask the user to visit a URL through an MCP URL-mode elicitation.', inputSchema: { type: 'object', properties: {} } },
]
createInterface({ input: process.stdin }).on('line', async line => {
  let m; try { m = JSON.parse(line) } catch { return }
  log({ in: m })
  if (m.id !== undefined && waiting.has(m.id) && !m.method) { waiting.get(m.id)(m); waiting.delete(m.id); return }
  if (m.method === 'initialize') return send({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'latch-elicit-probe', version: '1.0.0' } } })
  if (m.method === 'tools/list') return send({ jsonrpc: '2.0', id: m.id, result: { tools } })
  if (m.method === 'tools/call') {
    let reply
    if (m.params.name === 'approve') {
      const a = m.params.arguments || {}
      await new Promise(r => setTimeout(r, Number(process.env.APPROVE_DELAY_MS || 0)))
      const decision = process.env.APPROVE_MODE === 'allow' ? { behavior: 'allow', updatedInput: a.input } : { behavior: 'deny', message: 'Latch probe permission tool denied this.' }
      return send({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text', text: JSON.stringify(decision) }] } })
    }
    if (m.params.name === 'ask_form') {
      reply = await ask('elicitation/create', { mode: 'form', message: 'Latch probe: tell me how to label this run.', requestedSchema: { type: 'object', properties: {
        name: { type: 'string', title: 'Display name', description: 'Shown on the phone', minLength: 1, maxLength: 40 },
        count: { type: 'integer', title: 'Count', minimum: 1, maximum: 10, default: 2 },
        color: { type: 'string', title: 'Colour', enum: ['red', 'green', 'blue'], enumNames: ['Red', 'Green', 'Blue'] },
      }, required: ['name'] } })
    } else {
      reply = await ask('elicitation/create', { mode: 'url', message: 'Latch probe: sign in to the example service.', url: 'https://example.com/latch-probe/authorize?state=probe', elicitationId: 'latch-probe-url-1' })
    }
    return send({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text', text: 'elicitation reply: ' + JSON.stringify(reply.result ?? reply.error) }] } })
  }
  if (m.id !== undefined && m.method) send({ jsonrpc: '2.0', id: m.id, result: {} })
})
