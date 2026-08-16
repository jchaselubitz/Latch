import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { CloudflareTurnProvider } from './cloudflare-turn.ts';

const keyId = 'a'.repeat(32);
const apiToken = 't'.repeat(48);

function providerFor(iceServers: unknown) {
  return new CloudflareTurnProvider(keyId, apiToken, async () => new Response(JSON.stringify({ iceServers })));
}

describe('CloudflareTurnProvider', () => {
  it('accepts Cloudflare’s documented single-object iceServers response', async () => {
    const server = {
      urls: [
        'stun:stun.cloudflare.com:3478',
        'turn:turn.cloudflare.com:3478?transport=udp',
        'turns:turn.cloudflare.com:5349?transport=tcp',
      ],
      username: '1700000000:device',
      credential: 'credential-only-returned-to-device',
    };
    assert.deepEqual(await providerFor(server).issue(120), [server]);
  });

  it('continues to accept the array form used by older responses and test doubles', async () => {
    const servers = [
      { urls: ['stun:stun.cloudflare.com:3478'] },
      { urls: ['turn:turn.cloudflare.com:3478?transport=udp'], username: 'user', credential: 'password' },
    ];
    assert.deepEqual(await providerFor(servers).issue(120), servers);
  });

  it('rejects malformed provider responses instead of forwarding them to devices', async () => {
    await assert.rejects(() => providerFor({ urls: 'turn:not-an-array' }).issue(120));
  });

  it('has a relay-policy-independent STUN discovery configuration', () => {
    assert.deepEqual(new CloudflareTurnProvider(keyId, apiToken).stunServers(), [
      { urls: ['stun:stun.cloudflare.com:3478'] },
    ]);
  });
});
