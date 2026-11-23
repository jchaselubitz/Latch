// Loopback MCP server that demands OAuth, with local discovery and DCR. Logs every request.
import { createServer } from 'node:http'
import { appendFileSync, writeFileSync } from 'node:fs'
const LOG = process.env.OAUTH_LOG
const log = o => appendFileSync(LOG, JSON.stringify(o) + '\n')
const server = createServer((req, res) => {
  let body = ''
  req.on('data', c => body += c)
  req.on('end', () => {
    const base = `http://127.0.0.1:${server.address().port}`
    log({ method: req.method, url: req.url, auth: req.headers.authorization ? 'present' : undefined, body: body.slice(0, 2000) })
    const json = (code, o, headers = {}) => { res.writeHead(code, { 'content-type': 'application/json', ...headers }); res.end(JSON.stringify(o)) }
    if (req.url.startsWith('/.well-known/oauth-protected-resource')) return json(200, { resource: `${base}/mcp`, authorization_servers: [base] })
    if (req.url.startsWith('/.well-known/oauth-authorization-server') || req.url.startsWith('/.well-known/openid-configuration')) return json(200, { issuer: base, authorization_endpoint: `${base}/authorize`, token_endpoint: `${base}/token`, registration_endpoint: `${base}/register`, response_types_supported: ['code'], grant_types_supported: ['authorization_code', 'refresh_token'], code_challenge_methods_supported: ['S256'], token_endpoint_auth_methods_supported: ['none'] })
    if (req.url === '/register') { const r = JSON.parse(body || '{}'); return json(201, { client_id: 'latch-probe-client', redirect_uris: r.redirect_uris, token_endpoint_auth_method: 'none', grant_types: ['authorization_code'], response_types: ['code'] }) }
    if (req.url.startsWith('/mcp')) return json(401, { error: 'unauthorized' }, { 'www-authenticate': `Bearer resource_metadata="${base}/.well-known/oauth-protected-resource"` })
    json(404, { error: 'not found' })
  })
})
server.listen(0, '127.0.0.1', () => writeFileSync(process.env.OAUTH_PORT_FILE, String(server.address().port)))
