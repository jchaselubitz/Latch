import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const root = join(import.meta.dirname, '../../..');

test('canonical gateway discovery is protocol major 2', () => {
  const schema = JSON.parse(
    readFileSync(
      join(root, 'schemas/remote-access/v2/gateway-capabilities.schema.json'),
      'utf8'
    )
  ) as { properties: { protocolVersion: { const: number }; endpoints: { required: string[] } } };
  assert.equal(schema.properties.protocolVersion.const, 2);
  assert.deepEqual(schema.properties.endpoints.required, ['sessions', 'terminal', 'conversation']);
});

test('conversation contract reserves partial and represents ambiguous operations', () => {
  const item = readFileSync(
    join(root, 'schemas/remote-access/v2/conversation-item.schema.json'),
    'utf8'
  );
  const protocol = readFileSync(
    join(root, 'schemas/remote-access/v2/conversation-protocol.schema.json'),
    'utf8'
  );
  assert.match(item, /"partial"/);
  assert.match(protocol, /"operationEpoch"/);
  assert.match(protocol, /"ambiguous"/);
  assert.doesNotMatch(protocol, /conversation_reset/);
});
