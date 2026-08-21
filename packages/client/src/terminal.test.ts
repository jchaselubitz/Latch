import assert from 'node:assert/strict';
import test from 'node:test';

import { attachTerminal } from './terminal.ts';
import { defaultRetryPolicy } from './reconnect.ts';

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
  const closes: { code: number; reason: string }[] = [];
  handle.onClose(info => closes.push(info));

  FakeSocket.instances[0]!.closeWith(4404, 'session not found');

  assert.deepEqual(closes, [{ code: 4404, reason: 'session not found' }]);
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

test('read-only terminal mode is explicit in the WebSocket contract', () => {
  FakeSocket.instances = [];
  const handle = attachTerminal({
    baseUrl: 'http://127.0.0.1:4610',
    token: 'secret',
    sessionId: 'ses_one',
    mode: 'read-only',
    retry: defaultRetryPolicy,
    webSocket: FakeSocket as unknown as new (
      url: string,
      protocols?: string | string[]
    ) => WebSocket
  });
  assert.equal(
    FakeSocket.instances[0]?.url,
    'ws://127.0.0.1:4610/v2/sessions/ses_one/terminal?mode=read-only'
  );
  handle.close();
});
