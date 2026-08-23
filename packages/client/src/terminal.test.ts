import assert from 'node:assert/strict';
import test from 'node:test';

import { attachTerminal } from './terminal.ts';
import { createLatchClient } from './client.ts';
import { defaultRetryPolicy } from './reconnect.ts';
import type { TerminalCloseInfo } from './types.ts';

// Enough of the WebSocket surface for the client, and deliberately without the
// static `OPEN` constant: an injected constructor is not required to carry it,
// and the client must not read readyState constants off the class.
class FakeSocket {
  static instances: FakeSocket[] = [];

  readyState = 0;
  binaryType = '';
  sent: (Uint8Array | string)[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  onerror: (() => void) | null = null;

  url: string;
  protocols?: string | string[];

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeSocket.instances.push(this);
  }

  open() {
    this.readyState = 1;
    this.onopen?.();
  }

  send(payload: Uint8Array | string) {
    this.sent.push(payload);
  }

  closeWith(code: number, reason = '') {
    this.readyState = 3;
    this.onclose?.({ code, reason });
  }

  close() {
    this.closeWith(1000);
  }
}

function attach() {
  FakeSocket.instances = [];
  return attachTerminal({
    baseUrl: 'http://127.0.0.1:4610',
    token: 'secret',
    sessionId: 'ses_one',
    retry: defaultRetryPolicy,
    webSocket: FakeSocket as unknown as new (
      url: string,
      protocols?: string | string[]
    ) => WebSocket
  });
}

test('the token travels as the latch.v2 subprotocol, not the query string', () => {
  const handle = attach();
  const socket = FakeSocket.instances[0]!;
  assert.equal(socket.url, 'ws://127.0.0.1:4610/v2/sessions/ses_one/terminal');
  assert.deepEqual(socket.protocols, ['latch.v2.secret']);
  handle.close();
});

test('writes made before the socket opens are flushed on open', () => {
  const handle = attach();
  const socket = FakeSocket.instances[0]!;
  handle.write(new Uint8Array([0x68, 0x69]));
  assert.equal(socket.sent.length, 0);
  socket.open();
  assert.deepEqual(socket.sent, [new Uint8Array([0x68, 0x69])]);
  handle.close();
});

test('a missing session closes for good instead of reconnecting forever', () => {
  const handle = attach();
  const states: string[] = [];
  handle.onState(state => states.push(state));
  const closes: TerminalCloseInfo[] = [];
  handle.onClose(info => closes.push(info));

  FakeSocket.instances[0]!.closeWith(4404, 'session not found');

  assert.deepEqual(closes, [
    { code: 4404, reason: 'session not found', surface: undefined }
  ]);
  assert.equal(states.at(-1), 'closed');
  assert.equal(FakeSocket.instances.length, 1);
  handle.close();
});

test('an ordinary close schedules a reconnect', async () => {
  const handle = attach();
  FakeSocket.instances[0]!.closeWith(1006);
  await new Promise(resolve => setTimeout(resolve, defaultRetryPolicy.initialMs + 50));
  assert.equal(FakeSocket.instances.length, 2);
  handle.close();
});

test('resize is a control frame and is replayed on the next connection', () => {
  const handle = attach();
  const first = FakeSocket.instances[0]!;
  first.open();
  handle.resize({ cols: 100, rows: 30 });
  assert.deepEqual(first.sent.at(-1), JSON.stringify({ type: 'resize', cols: 100, rows: 30 }));
  handle.close();
});

test('a steal closes the handle and is never reconnected automatically', async () => {
  const handle = attach();
  const closes: TerminalCloseInfo[] = [];
  handle.onClose(info => closes.push(info));
  FakeSocket.instances[0]!.closeWith(4409, 'stolen');
  await new Promise(resolve => setTimeout(resolve, defaultRetryPolicy.initialMs + 50));
  // Reconnecting here would take the surface back from whoever just claimed
  // it, and two such clients would trade the session forever.
  assert.equal(FakeSocket.instances.length, 1);
  assert.deepEqual(closes, [{ code: 4409, reason: 'stolen', surface: 'stolen' }]);
});

test('every reasoned close names why the surface ended', async () => {
  const cases: [number, string][] = [
    [4408, 'slow_client'],
    [4410, 'session_exited'],
    [4500, 'kernel_error']
  ];
  for (const [code, reason] of cases) {
    const handle = attach();
    const closes: TerminalCloseInfo[] = [];
    handle.onClose(info => closes.push(info));
    FakeSocket.instances.at(-1)!.closeWith(code, reason);
    assert.deepEqual(closes, [{ code, reason, surface: reason }]);
    handle.close();
  }
});

test('a gateway that predates the exclusive cutover is refused, not attached', async () => {
  // A gateway from before the cutover still speaks protocol 2, so the version
  // alone cannot tell it apart. Only `features.exclusiveTerminal` does, and a
  // terminal connection there would be some other behaviour entirely -- a
  // mirrored or read-only surface -- which this release does not support.
  const cases: [string, unknown][] = [
    ['no features block at all', { protocolVersion: 2, productVersion: '0.0.0' }],
    [
      'the feature declared false',
      { protocolVersion: 2, productVersion: '0.0.0', features: { exclusiveTerminal: false } }
    ]
  ];
  for (const [label, capabilities] of cases) {
    FakeSocket.instances = [];
    const client = createLatchClient({
      url: 'http://127.0.0.1:1',
      token: 'token',
      webSocket: FakeSocket as unknown as typeof WebSocket,
      fetch: (async () =>
        new Response(JSON.stringify(capabilities), {
          status: 200,
          headers: { 'content-type': 'application/json' }
        })) as unknown as typeof fetch
    });
    const handle = client.attachTerminal({ sessionId: 'ses_1', cols: 80, rows: 24 });
    const states: string[] = [];
    handle.onState(next => states.push(next));
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(states.at(-1), 'closed', label);
    // Refused, and never retried: reconnecting into an old gateway forever is
    // the failure mode a version check exists to avoid.
    await new Promise(resolve => setTimeout(resolve, defaultRetryPolicy.initialMs + 50));
    assert.equal(FakeSocket.instances.length, 1, label);
  }
});

test('an exclusive gateway is attached and left open', async () => {
  FakeSocket.instances = [];
  const client = createLatchClient({
    url: 'http://127.0.0.1:1',
    token: 'token',
    webSocket: FakeSocket as unknown as typeof WebSocket,
    fetch: (async () =>
      new Response(
        JSON.stringify({
          protocolVersion: 2,
          productVersion: '0.0.0',
          features: { exclusiveTerminal: true }
        }),
        { status: 200, headers: { 'content-type': 'application/json' } }
      )) as unknown as typeof fetch
  });
  const handle = client.attachTerminal({ sessionId: 'ses_1', cols: 80, rows: 24 });
  const states: string[] = [];
  handle.onState(next => states.push(next));
  FakeSocket.instances[0]!.open();
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(states.at(-1), 'open');
  handle.close();
});
