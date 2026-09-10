import assert from 'node:assert/strict';
import test from 'node:test';

import {
  createLatchClient,
  GATEWAY_ERROR_STOP_UNSUPPORTED,
  LatchGatewayError
} from './client.ts';

type Call = { url: string; method: string; headers: Record<string, string>; body: unknown };

/// A gateway made of canned answers keyed by `METHOD path`, recording what it
/// was asked so a test can pin the wire shape and not only the result.
function gateway(answers: Record<string, () => Response>) {
  const calls: Call[] = [];
  const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? 'GET';
    calls.push({
      url,
      method,
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: init?.body
    });
    const path = new URL(url).pathname;
    const answer = answers[`${method} ${path}`];
    return answer ? answer() : new Response('not found', { status: 404 });
  }) as unknown as typeof fetch;
  return { calls, fetchImpl };
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' }
  });
}

const advertisingStop = () =>
  json({
    protocolVersion: 2,
    productVersion: '0.0.0',
    endpoints: { sessions: true, terminal: true, conversation: true, stopSession: true },
    features: { exclusiveTerminal: true }
  });

test('stopSession posts to the stop route with the bearer token and no body', async () => {
  const { calls, fetchImpl } = gateway({
    'GET /v2/capabilities': advertisingStop,
    'POST /v2/sessions/ses_1/stop': () => json({ id: 'ses_1', state: 'exited', stopped: true })
  });
  const client = createLatchClient({ url: 'http://127.0.0.1:1/', token: 'secret', fetch: fetchImpl });

  const report = await client.stopSession({ sessionId: 'ses_1' });

  assert.deepEqual(report, { id: 'ses_1', state: 'exited', stopped: true });
  const stop = calls.find(call => call.method === 'POST');
  assert.ok(stop);
  assert.equal(stop.url, 'http://127.0.0.1:1/v2/sessions/ses_1/stop');
  assert.equal(stop.headers.Authorization, 'Bearer secret');
  // There is nothing to choose: no signal, force flag, or timeout travels.
  assert.equal(stop.body, undefined);
});

test('a session id is path-encoded rather than spliced into the route', async () => {
  const { calls, fetchImpl } = gateway({
    'GET /v2/capabilities': advertisingStop,
    'POST /v2/sessions/a%2Fb/stop': () => json({ id: 'a/b', state: 'exited', stopped: true })
  });
  const client = createLatchClient({ url: 'http://127.0.0.1:1', token: 't', fetch: fetchImpl });
  await client.stopSession({ sessionId: 'a/b' });
  assert.ok(calls.some(call => call.url.endsWith('/v2/sessions/a%2Fb/stop')));
});

test('a gateway that does not advertise stop is refused before it is asked', async () => {
  const { calls, fetchImpl } = gateway({
    'GET /v2/capabilities': () =>
      json({
        protocolVersion: 2,
        productVersion: '0.0.0',
        endpoints: { sessions: true, terminal: true, conversation: true },
        features: { exclusiveTerminal: true }
      })
  });
  const client = createLatchClient({ url: 'http://127.0.0.1:1', token: 't', fetch: fetchImpl });

  await assert.rejects(client.stopSession({ sessionId: 'ses_1' }), (error: unknown) => {
    assert.ok(error instanceof LatchGatewayError);
    assert.equal(error.code, GATEWAY_ERROR_STOP_UNSUPPORTED);
    return true;
  });
  // The refusal is the client's own: an old gateway's bare 404 on this path
  // must never be read as the session being gone.
  assert.ok(calls.every(call => call.method === 'GET'));
});

test('gateway error codes reach the caller as codes, not as prose', async () => {
  const cases: [string, number, string][] = [
    ['session_still_running', 409, 'the session did not stop'],
    ['session_not_found', 404, 'session not found']
  ];
  for (const [code, status, reason] of cases) {
    const { fetchImpl } = gateway({
      'GET /v2/capabilities': advertisingStop,
      'POST /v2/sessions/ses_1/stop': () => json({ error: code, reason }, status)
    });
    const client = createLatchClient({ url: 'http://127.0.0.1:1', token: 't', fetch: fetchImpl });
    await assert.rejects(client.stopSession({ sessionId: 'ses_1' }), (error: unknown) => {
      assert.ok(error instanceof LatchGatewayError);
      assert.equal(error.code, code);
      assert.equal(error.status, status);
      assert.match(error.message, new RegExp(reason));
      return true;
    });
  }
});

test('an answer without a code falls back to request_failed and keeps its text', async () => {
  const { fetchImpl } = gateway({
    'GET /v2/sessions': () => new Response('gateway is restarting', { status: 503 })
  });
  const client = createLatchClient({ url: 'http://127.0.0.1:1', token: 't', fetch: fetchImpl });
  await assert.rejects(client.listSessions(), (error: unknown) => {
    assert.ok(error instanceof LatchGatewayError);
    assert.equal(error.code, 'request_failed');
    assert.equal(error.status, 503);
    assert.match(error.message, /gateway is restarting/);
    return true;
  });
});
