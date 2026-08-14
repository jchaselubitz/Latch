import assert from 'node:assert/strict';
import test from 'node:test';

import { legacyGatewayCapabilities, supportsGatewayEndpoint } from './compatibility.ts';
import { createLatchClient } from './client.ts';

test('a pre-discovery gateway degrades to sessions and terminal', () => {
  const legacy = legacyGatewayCapabilities();
  assert.equal(supportsGatewayEndpoint({ capabilities: legacy, endpoint: 'terminal' }), true);
  assert.equal(supportsGatewayEndpoint({ capabilities: legacy, endpoint: 'events' }), false);
  assert.equal(supportsGatewayEndpoint({ capabilities: legacy, endpoint: 'send' }), false);
});

test('a client does not guess at a different protocol major', () => {
  const capabilities = legacyGatewayCapabilities();
  capabilities.protocolVersion = 2;
  capabilities.endpoints.events = true;
  assert.equal(supportsGatewayEndpoint({ capabilities, endpoint: 'events' }), false);
});

test('gateway discovery treats a 404 as the terminal-only legacy surface', async () => {
  const client = createLatchClient({
    url: 'http://127.0.0.1:4610',
    token: 'token',
    fetch: async () => new Response(JSON.stringify({ error: 'not found' }), { status: 404 })
  });
  const capabilities = await client.gatewayCapabilities();
  assert.equal(capabilities.productVersion, 'unknown');
  assert.equal(capabilities.endpoints.terminal, true);
  assert.equal(capabilities.endpoints.events, false);
});
