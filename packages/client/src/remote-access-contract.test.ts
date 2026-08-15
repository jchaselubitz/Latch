import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import type { GatewayCapabilities } from './types.ts';
import type { GatewayReadiness, TerminalAccessMode } from './generated.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const fixture = (name: string) =>
  readFileSync(join(root, 'fixtures/remote-access/v1', name), 'utf8');

test('v1 fixtures retain the canonical discovery and readiness shape', () => {
  const capabilities = JSON.parse(fixture('gateway-capabilities.json')) as GatewayCapabilities;
  const readiness = JSON.parse(fixture('gateway-readiness.json')) as GatewayReadiness;
  const terminal = JSON.parse(fixture('terminal-read-only.json')) as {
    mode: TerminalAccessMode;
    cols: number;
    rows: number;
  };

  assert.equal(capabilities.protocolVersion, 1);
  assert.deepEqual(capabilities.features, { idempotencyKeys: true, readOnlyTerminal: true });
  assert.match(capabilities.gatewayInstanceId, /^gw-[0-9a-f]+-[0-9a-f]+$/);
  assert.equal(readiness.formatVersion, 1);
  assert.equal(readiness.protocolVersion, capabilities.protocolVersion);
  assert.equal(readiness.gatewayInstanceId, capabilities.gatewayInstanceId);
  assert.equal(terminal.mode, 'read-only');
  assert.equal(terminal.cols, 80);
});

test('every remote-access schema has a versioned canonical id', () => {
  const names = [
    'gateway-capabilities.schema.json',
    'gateway-readiness.schema.json',
    'terminal-connection.schema.json',
    'idempotency-key.schema.json',
    'send-request.schema.json'
  ];
  for (const name of names) {
    const schema = JSON.parse(
      readFileSync(join(root, 'schemas/remote-access/v1', name), 'utf8')
    ) as { $id: string };
    assert.equal(
      schema.$id,
      `https://latch.cooperativ.dev/schemas/remote-access/v1/${name}`
    );
  }
});
