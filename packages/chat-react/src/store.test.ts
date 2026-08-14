import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import type { HarnessEvent } from '@latch/harness-schema';

import { applyEvent, emptyTranscript, foldEvents } from './store.ts';

const fixtures = join(
  dirname(fileURLToPath(import.meta.url)),
  '../../../fixtures/harness/claude-code'
);

function loadExpected({ name }: { name: string }): HarnessEvent[] {
  const raw = readFileSync(join(fixtures, name, 'expected.ndjson'), 'utf8');
  return raw
    .split('\n')
    .filter(line => line.trim().length > 0)
    .map(line => JSON.parse(line) as HarnessEvent);
}

test('conversation fixture folds into user, assistant, and paired tool cards', () => {
  const state = foldEvents({ events: loadExpected({ name: 'conversation' }) });
  assert.equal(state.cursor, 6);
  assert.equal(state.connectorEpoch, 1);
  assert.equal(state.status, 'idle');
  assert.deepEqual(
    state.items.map(item => item.kind),
    ['user', 'assistant', 'tool', 'assistant']
  );
  const tool = state.items[2];
  assert.equal(tool?.kind, 'tool');
  if (tool?.kind === 'tool') {
    assert.equal(tool.tool, 'Bash');
    assert.equal(tool.finished, true);
    assert.deepEqual(tool.output, { available: true });
  }
  const last = state.items.at(-1);
  assert.equal(last?.kind, 'assistant');
  if (last?.kind === 'assistant') {
    assert.equal(last.text, 'The build passes.');
    assert.equal(last.streaming, false);
  }
});

test('assistant_delta accumulates into the open turn until assistant_message', () => {
  const base = {
    sessionId: 's',
    at: '2026-08-14T00:00:00Z',
    harnessVersion: '2.1.119',
    connectorEpoch: 1
  } as const;
  let state = emptyTranscript();
  state = applyEvent({
    state,
    event: { ...base, type: 'assistant_delta', text: 'Hel' }
  });
  state = applyEvent({
    state,
    event: { ...base, type: 'assistant_delta', text: 'lo' }
  });
  assert.equal(state.items.length, 1);
  const streaming = state.items[0];
  assert.equal(streaming?.kind, 'assistant');
  if (streaming?.kind === 'assistant') {
    assert.equal(streaming.text, 'Hello');
    assert.equal(streaming.streaming, true);
  }
  state = applyEvent({
    state,
    event: { ...base, type: 'assistant_message', text: 'Hello' }
  });
  const done = state.items[0];
  assert.equal(done?.kind, 'assistant');
  if (done?.kind === 'assistant') {
    assert.equal(done.streaming, false);
    assert.equal(done.text, 'Hello');
  }
});

test('permission fixture opens a pending-request entry', () => {
  const state = foldEvents({ events: loadExpected({ name: 'permission-request' }) });
  assert.equal(state.pendingRequestId, 'permission-1');
  const item = state.items[0];
  assert.equal(item?.kind, 'awaiting_input');
  if (item?.kind === 'awaiting_input') {
    assert.equal(item.requestKind, 'permission');
    assert.deepEqual(item.choices, ['Allow once', 'Deny']);
  }
});

test('question fixture pairs AskUserQuestion with awaiting_input', () => {
  const state = foldEvents({ events: loadExpected({ name: 'awaiting-question' }) });
  assert.equal(state.pendingRequestId, 'question-1');
  assert.equal(state.items[0]?.kind, 'tool');
  const prompt = state.items[1];
  assert.equal(prompt?.kind, 'awaiting_input');
  if (prompt?.kind === 'awaiting_input') {
    assert.equal(prompt.requestKind, 'question');
    assert.deepEqual(prompt.choices, ['Postgres', 'SQLite']);
  }
});

test('a connector-epoch change drops the previous timeline', () => {
  const first: HarnessEvent = {
    sessionId: 's',
    at: '2026-08-14T00:00:00Z',
    harnessVersion: '2.1.119',
    connectorEpoch: 1,
    type: 'user_message',
    text: 'old'
  };
  const shifted: HarnessEvent = {
    ...first,
    connectorEpoch: 2,
    text: 'new'
  };
  const state = foldEvents({ events: [first, shifted] });
  assert.equal(state.connectorEpoch, 2);
  assert.equal(state.items.length, 1);
  assert.equal(state.items[0]?.kind, 'user');
  if (state.items[0]?.kind === 'user') {
    assert.equal(state.items[0].text, 'new');
  }
});

test('a later event retires the open request and marks the prompt answered', () => {
  const base = {
    sessionId: 's',
    at: '2026-08-14T00:00:00Z',
    harnessVersion: '2.1.119',
    connectorEpoch: 1
  } as const;
  let state = applyEvent({
    state: emptyTranscript(),
    event: {
      ...base,
      type: 'awaiting_input',
      requestId: 'permission-1',
      kind: 'permission',
      prompt: 'Install the test runner',
      choices: ['Allow once', 'Deny']
    }
  });
  assert.equal(state.pendingRequestId, 'permission-1');
  const open = state.items[0];
  assert.equal(open?.kind, 'awaiting_input');
  if (open?.kind === 'awaiting_input') {
    assert.equal(open.resolved, false);
  }

  state = applyEvent({
    state,
    event: { ...base, type: 'tool_started', tool: 'Bash', input: { command: 'npm test' } }
  });
  assert.equal(state.pendingRequestId, null);
  const answered = state.items[0];
  assert.equal(answered?.kind, 'awaiting_input');
  if (answered?.kind === 'awaiting_input') {
    assert.equal(answered.resolved, true);
  }
});

test('a second request retires the first and becomes the pending one', () => {
  const base = {
    sessionId: 's',
    at: '2026-08-14T00:00:00Z',
    harnessVersion: '2.1.119',
    connectorEpoch: 1,
    type: 'awaiting_input',
    kind: 'permission',
    prompt: 'Run it'
  } as const;
  const state = foldEvents({
    events: [
      { ...base, requestId: 'permission-1' },
      { ...base, requestId: 'permission-2' }
    ]
  });
  assert.equal(state.pendingRequestId, 'permission-2');
  assert.deepEqual(
    state.items.map(item => (item.kind === 'awaiting_input' ? item.resolved : null)),
    [true, false]
  );
});
