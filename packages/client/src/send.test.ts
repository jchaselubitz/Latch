import assert from 'node:assert/strict';
import test from 'node:test';

import { createLatchClient, LatchSendError } from './client.ts';
import type { SessionCapabilities } from './types.ts';

type FetchCall = {
  url: string;
  method: string;
  body?: string;
  idempotencyKey?: string | null;
};

function jsonResponse({
  status = 200,
  body
}: {
  status?: number;
  body: unknown;
}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' }
  });
}

function capabilities(overrides: Partial<SessionCapabilities> = {}): SessionCapabilities {
  return {
    sendMessage: true,
    sendKeys: true,
    resolve: false,
    canSend: { ok: true },
    events: { ok: false, reason: 'no harness connector' },
    ...overrides
  };
}

function clientWith({
  fetchImpl
}: {
  fetchImpl: typeof fetch;
}) {
  return createLatchClient({
    url: 'http://127.0.0.1:4610',
    token: 'secret',
    fetch: fetchImpl
  });
}

test('send posts the message body without a capabilities preflight', async () => {
  const calls: FetchCall[] = [];
  const client = clientWith({
    fetchImpl: async (input, init) => {
      const url = String(input);
      const method = init?.method ?? 'GET';
      const body = typeof init?.body === 'string' ? init.body : undefined;
      calls.push({
        url,
        method,
        body,
        idempotencyKey: new Headers(init?.headers).get('Idempotency-Key')
      });
      assert.equal(method, 'POST');
      return jsonResponse({
        body: { sessionId: 'ses_one', operation: 'message', sent: true, resolved: false }
      });
    }
  });

  const report = await client.send({ sessionId: 'ses_one', message: 'hello' });
  assert.equal(report.sent, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0]?.url, 'http://127.0.0.1:4610/v1/sessions/ses_one/send');
  assert.equal(calls[0]?.method, 'POST');
  assert.deepEqual(JSON.parse(calls[0]?.body ?? '{}'), { message: 'hello' });
});

test('message retries can reuse a caller-supplied idempotency key', async () => {
  const calls: FetchCall[] = [];
  const client = clientWith({
    fetchImpl: async (input, init) => {
      calls.push({
        url: String(input),
        method: init?.method ?? 'GET',
        idempotencyKey: new Headers(init?.headers).get('Idempotency-Key')
      });
      return jsonResponse({
        body: { sessionId: 'ses_one', operation: 'message', sent: true, resolved: false }
      });
    }
  });

  await client.send({ sessionId: 'ses_one', message: 'hello', idempotencyKey: 'retry-1' });
  assert.equal(calls[0]?.idempotencyKey, 'retry-1');
});

test('send still posts when canSend.ok is false so the gateway is the authority', async () => {
  const calls: FetchCall[] = [];
  const client = clientWith({
    fetchImpl: async (input, init) => {
      const url = String(input);
      const method = init?.method ?? 'GET';
      calls.push({ url, method, body: typeof init?.body === 'string' ? init.body : undefined });
      assert.equal(method, 'POST');
      return jsonResponse({
        status: 409,
        body: {
          error: 'refused',
          reason: 'not a known Claude Code harness'
        }
      });
    }
  });

  await assert.rejects(
    () => client.send({ sessionId: 'ses_one', message: 'hello' }),
    (error: unknown) => {
      assert.ok(error instanceof LatchSendError);
      assert.equal(error.refused, true);
      assert.equal(error.reason, 'not a known Claude Code harness');
      return true;
    }
  );
  assert.equal(calls.some(call => call.method === 'POST'), true);
});

test('send posts keys even when the composer would be disabled', async () => {
  const calls: FetchCall[] = [];
  const client = clientWith({
    fetchImpl: async (input, init) => {
      const url = String(input);
      const method = init?.method ?? 'GET';
      calls.push({ url, method, body: typeof init?.body === 'string' ? init.body : undefined });
      return jsonResponse({
        body: { sessionId: 'ses_one', operation: 'keys', sent: true, resolved: false }
      });
    }
  });

  const report = await client.send({ sessionId: 'ses_one', keys: 'C-c' });
  assert.equal(report.operation, 'keys');
  assert.equal(calls[0]?.method, 'POST');
  assert.deepEqual(JSON.parse(calls[0]?.body ?? '{}'), { keys: 'C-c' });
});

test('send surfaces a 409 reason from the gateway', async () => {
  const client = clientWith({
    fetchImpl: async (_input, init) => {
      assert.equal(init?.method, 'POST');
      return jsonResponse({
        status: 409,
        body: {
          error: 'refused',
          reason: 'request `permission-1` is stale; the visible request is `permission-2`'
        }
      });
    }
  });

  await assert.rejects(
    () =>
      client.send({
        sessionId: 'ses_one',
        resolve: { requestId: 'permission-1', choice: 'Allow once' }
      }),
    (error: unknown) => {
      assert.ok(error instanceof LatchSendError);
      assert.equal(error.status, 409);
      assert.equal(error.refused, true);
      assert.match(error.reason, /stale/);
      return true;
    }
  );
});

test('canSend is the session capabilities document', async () => {
  const client = clientWith({
    fetchImpl: async () => jsonResponse({ body: capabilities({ sendMessage: false }) })
  });
  const report = await client.canSend({ sessionId: 'ses_one' });
  assert.equal(report.sendMessage, false);
  assert.equal(report.canSend.ok, true);
});
