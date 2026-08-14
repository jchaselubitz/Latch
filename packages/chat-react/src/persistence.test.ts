import assert from 'node:assert/strict';
import test from 'node:test';

import type { HarnessEvent } from '@latch/harness-schema';

import {
  decodeSnapshot,
  encodeSnapshot,
  memoryTranscriptStorage,
  restoreTranscript,
  transcriptStorageKey,
  webTranscriptStorage,
  type WebStorageLike
} from './persistence.ts';
import { applyEvent, emptyTranscript } from './store.ts';

const event: HarnessEvent = {
  sessionId: 'ses_one',
  at: '2026-08-14T00:00:00Z',
  harnessVersion: '2.1.119',
  connectorEpoch: 1,
  type: 'user_message',
  text: 'ship it'
};

function fakeStorage(): WebStorageLike & { entries: Map<string, string> } {
  const entries = new Map<string, string>();
  return {
    entries,
    getItem: key => entries.get(key) ?? null,
    setItem: (key, value) => {
      entries.set(key, value);
    },
    removeItem: key => {
      entries.delete(key);
    }
  };
}

test('a stored snapshot restores the timeline and its cursor', () => {
  const storage = memoryTranscriptStorage();
  const key = transcriptStorageKey({ sessionId: 'ses_one' });
  const state = applyEvent({ state: emptyTranscript(), event });
  storage.save(key, state);

  const restored = restoreTranscript({ storage, key });
  assert.equal(restored.cursor, 1);
  assert.equal(restored.connectorEpoch, 1);
  assert.deepEqual(restored.items, state.items);
});

test('no storage and an empty slot both start from cursor 0', () => {
  const key = transcriptStorageKey({ sessionId: 'ses_one' });
  assert.deepEqual(restoreTranscript({ storage: null, key }), emptyTranscript());
  assert.deepEqual(
    restoreTranscript({ storage: memoryTranscriptStorage(), key }),
    emptyTranscript()
  );
});

test('a cursor with no epoch is dropped rather than resumed', () => {
  const storage = memoryTranscriptStorage();
  const key = transcriptStorageKey({ sessionId: 'ses_one' });
  storage.save(key, { ...emptyTranscript(), cursor: 7 });

  // Resuming at index 7 without knowing which epoch minted it would splice a
  // possibly shifted stream onto the old timeline.
  assert.deepEqual(restoreTranscript({ storage, key }), emptyTranscript());
  assert.equal(storage.load(key), null);
});

test('a snapshot from another envelope version is discarded and cleared', () => {
  const backing = fakeStorage();
  const storage = webTranscriptStorage({ storage: backing });
  const key = transcriptStorageKey({ sessionId: 'ses_one' });
  backing.setItem(key, JSON.stringify({ version: 0, state: emptyTranscript() }));

  assert.equal(storage.load(key), null);
  assert.equal(backing.entries.has(key), false);
});

test('corrupt and malformed snapshots decode to null', () => {
  assert.equal(decodeSnapshot({ raw: null }), null);
  assert.equal(decodeSnapshot({ raw: 'not json' }), null);
  assert.equal(decodeSnapshot({ raw: JSON.stringify({ version: 1 }) }), null);
  assert.equal(
    decodeSnapshot({ raw: JSON.stringify({ version: 1, state: { items: [], cursor: -1 } }) }),
    null
  );
  assert.equal(
    decodeSnapshot({
      raw: JSON.stringify({ version: 1, state: { items: 'nope', cursor: 0 } })
    }),
    null
  );
});

test('a snapshot round-trips through encode and decode', () => {
  const state = applyEvent({ state: emptyTranscript(), event });
  assert.deepEqual(decodeSnapshot({ raw: encodeSnapshot({ state }) }), state);
});

test('storage that throws never breaks the stream', () => {
  const storage = webTranscriptStorage({
    storage: {
      getItem: () => {
        throw new Error('private mode');
      },
      setItem: () => {
        throw new Error('quota exceeded');
      },
      removeItem: () => {
        throw new Error('private mode');
      }
    }
  });
  const key = transcriptStorageKey({ sessionId: 'ses_one' });
  assert.equal(storage.load(key), null);
  storage.save(key, emptyTranscript());
  storage.clear(key);
});
