import assert from 'node:assert/strict';
import test from 'node:test';

import { subscribeEvents } from './events.ts';
import { defaultRetryPolicy } from './reconnect.ts';
import { NO_CONNECTOR_CLOSE, STALE_CURSOR_CLOSE } from './ws.ts';

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

  emit(data: string) {
    this.onmessage?.({ data });
  }

  closeWith(code: number, reason = '') {
    this.readyState = 3;
    this.onclose?.({ code, reason });
  }

  close() {
    this.closeWith(1000);
  }
}

function subscribe({ cursor = 0, connectorEpoch }: { cursor?: number; connectorEpoch?: number } = {}) {
  FakeSocket.instances = [];
  return subscribeEvents({
    baseUrl: 'http://127.0.0.1:4610',
    token: 'secret',
    sessionId: 'ses_one',
    cursor,
    connectorEpoch,
    retry: defaultRetryPolicy,
    webSocket: FakeSocket as unknown as new (
      url: string,
      protocols?: string | string[]
    ) => WebSocket
  });
}

const sampleEvent = {
  sessionId: 'claude-session-1',
  at: '2026-08-13T10:00:00Z',
  harnessVersion: '2.1.119',
  connectorEpoch: 1,
  type: 'user_message',
  text: 'Check the build.'
};

test('events use the latch.v1 subprotocol and a cursor query', () => {
  const sub = subscribe({ cursor: 4 });
  const socket = FakeSocket.instances[0]!;
  assert.equal(socket.url, 'ws://127.0.0.1:4610/v1/sessions/ses_one/events?cursor=4');
  assert.deepEqual(socket.protocols, ['latch.v1.secret']);
  sub.close();
});

test('yields each text frame as a HarnessEvent and advances the cursor', async () => {
  const sub = subscribe();
  FakeSocket.instances[0]!.open();
  FakeSocket.instances[0]!.emit(JSON.stringify(sampleEvent));
  const first = await sub[Symbol.asyncIterator]().next();
  assert.deepEqual(first.value, sampleEvent);
  assert.equal(sub.cursor, 1);
  sub.close();
});

test('a missing connector closes for good', () => {
  const sub = subscribe();
  const closes: { code: number; reason: string }[] = [];
  sub.onClose(info => closes.push(info));
  FakeSocket.instances[0]!.closeWith(NO_CONNECTOR_CLOSE, 'no harness connector');
  assert.deepEqual(closes, [{ code: NO_CONNECTOR_CLOSE, reason: 'no harness connector' }]);
  assert.equal(FakeSocket.instances.length, 1);
  sub.close();
});

test('a stale cursor resyncs from zero instead of failing', async () => {
  const sub = subscribe({ cursor: 9, connectorEpoch: 1 });
  const resyncs: number[] = [];
  sub.onResync(() => resyncs.push(sub.cursor));
  FakeSocket.instances[0]!.closeWith(STALE_CURSOR_CLOSE, 'stale cursor');
  assert.deepEqual(resyncs, [0]);
  await new Promise(resolve => setTimeout(resolve, defaultRetryPolicy.initialMs + 50));
  assert.equal(FakeSocket.instances.length, 2);
  assert.equal(
    FakeSocket.instances[1]!.url,
    'ws://127.0.0.1:4610/v1/sessions/ses_one/events?cursor=0'
  );
  sub.close();
});

test('an epoch mismatch on a resumed cursor resyncs from zero', async () => {
  const sub = subscribe({ cursor: 4, connectorEpoch: 1 });
  const resyncs: number[] = [];
  sub.onResync(() => resyncs.push(1));
  FakeSocket.instances[0]!.open();
  FakeSocket.instances[0]!.emit(
    JSON.stringify({ ...sampleEvent, connectorEpoch: 2, text: 'shifted' })
  );
  assert.deepEqual(resyncs, [1]);
  assert.equal(sub.cursor, 0);
  sub.close();
});
