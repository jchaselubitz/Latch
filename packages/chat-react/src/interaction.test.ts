import assert from 'node:assert/strict';
import test from 'node:test';

import type { SessionCapabilities } from '@latch/client';

import { composerBlockReason, promptBlockReason } from './interaction.ts';

function capabilities(overrides: Partial<SessionCapabilities> = {}): SessionCapabilities {
  return {
    sendMessage: true,
    sendKeys: true,
    resolve: false,
    canSend: { ok: true },
    events: { ok: true, harness: 'claude', connectorEpoch: 1 },
    ...overrides
  };
}

test('composer disables with the canSend reason', () => {
  assert.equal(
    composerBlockReason({
      capabilities: capabilities({
        sendMessage: false,
        canSend: { ok: false, reason: 'the Claude Code composer already contains text' }
      })
    }),
    'the Claude Code composer already contains text'
  );
});

test('composer disables while capabilities are still loading', () => {
  assert.equal(
    composerBlockReason({ capabilities: null }),
    'checking whether this session can accept a message'
  );
});

test('composer disables when sendMessage is false even if canSend.ok', () => {
  assert.equal(
    composerBlockReason({
      capabilities: capabilities({ sendMessage: false, resolve: true })
    }),
    'free-text send is not available on the current screen'
  );
});

test('a prompt widget cannot answer a later requestId', () => {
  assert.equal(
    promptBlockReason({
      capabilities: capabilities({ resolve: true }),
      requestId: 'permission-1',
      activeRequestId: 'permission-2'
    }),
    'this prompt is no longer the visible request'
  );
});

test('a prompt widget is enabled for its own requestId when resolve is advertised', () => {
  assert.equal(
    promptBlockReason({
      capabilities: capabilities({ resolve: true }),
      requestId: 'permission-1',
      activeRequestId: 'permission-1'
    }),
    null
  );
});

test('a prompt widget stays disabled when the harness does not advertise resolve', () => {
  assert.equal(
    promptBlockReason({
      capabilities: capabilities({
        resolve: false,
        canSend: { ok: false, reason: 'not a known Claude Code harness' }
      }),
      requestId: 'permission-1'
    }),
    'not a known Claude Code harness'
  );
});
