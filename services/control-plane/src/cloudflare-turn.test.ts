import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { CloudflareTurnProvider } from './cloudflare-turn.ts';

const keyId = 'a'.repeat(32);
const apiToken = 't'.repeat(48);

function providerFor(iceServers: unknown) {
  return new CloudflareTurnProvider(keyId, apiToken, async () => new Response(JSON.stringify({ iceServers })));
}

/** Records the single call `issue` or `revoke` made, and answers it. */
function recordingProvider(response: () => Response) {
  const calls: { url: string; init: RequestInit | undefined }[] = [];
  const provider = new CloudflareTurnProvider(keyId, apiToken, async (url, init) => {
    calls.push({ url: String(url), init });
    return response();
  });
  return { provider, calls };
}

describe('CloudflareTurnProvider', () => {
  it('accepts the array of ICE servers Cloudflare returns today', async () => {
    // The exact body from Cloudflare's generate-ice-servers documentation:
    // one entry for STUN and one for TURN, with the credential on the TURN
    // entry only.
    const servers = [
      { urls: ['stun:stun.cloudflare.com:3478', 'stun:stun.cloudflare.com:53'] },
      {
        urls: [
          'turn:turn.cloudflare.com:3478?transport=udp',
          'turn:turn.cloudflare.com:3478?transport=tcp',
          'turns:turn.cloudflare.com:5349?transport=tcp',
          'turns:turn.cloudflare.com:443?transport=tcp',
        ],
        username: '1700000000:device',
        credential: 'credential-only-returned-to-device',
      },
    ];
    assert.deepEqual(await providerFor(servers).issue(120), servers);
  });

  it('accepts a bare object, because the same field has been documented unwrapped', async () => {
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

  it('rejects malformed provider responses instead of forwarding them to devices', async () => {
    await assert.rejects(() => providerFor({ urls: 'turn:not-an-array' }).issue(120));
    await assert.rejects(() => providerFor(undefined).issue(120));
    await assert.rejects(
      () => providerFor([{ urls: ['turn:turn.cloudflare.com:3478'], credential: 7 }]).issue(120),
    );
  });

  it('reports a rejected key rather than issuing nothing quietly', async () => {
    // An unrecognised key id answers 404 with `cannot find specified key`,
    // not 401. Turning every non-2xx into a throw is what lets the API map a
    // misconfigured relay onto `relay_unavailable` for the device.
    const { provider } = recordingProvider(
      () => new Response(JSON.stringify({ error: 'cannot find specified key' }), { status: 404 }),
    );
    await assert.rejects(() => provider.issue(120), /issuance failed \(404\)/);
  });

  it('asks for credentials at the documented endpoint with the requested lifetime', async () => {
    const { provider, calls } = recordingProvider(
      () => new Response(JSON.stringify({ iceServers: [{ urls: ['stun:stun.cloudflare.com:3478'] }] }), { status: 201 }),
    );
    await provider.issue(120);
    assert.equal(calls.length, 1);
    assert.equal(
      calls[0]?.url,
      `https://rtc.live.cloudflare.com/v1/turn/keys/${keyId}/credentials/generate-ice-servers`,
    );
    assert.equal(calls[0]?.init?.method, 'POST');
    assert.deepEqual(calls[0]?.init?.headers, {
      authorization: `Bearer ${apiToken}`,
      'content-type': 'application/json',
    });
    assert.deepEqual(JSON.parse(String(calls[0]?.init?.body)), { ttl: 120 });
  });

  it('revokes by username and treats the documented empty 204 as success', async () => {
    const { provider, calls } = recordingProvider(() => new Response(null, { status: 204 }));
    await provider.revoke('1700000000:device');
    assert.equal(
      calls[0]?.url,
      `https://rtc.live.cloudflare.com/v1/turn/keys/${keyId}/credentials/1700000000%3Adevice/revoke`,
    );
    assert.equal(calls[0]?.init?.method, 'POST');
  });

  it('reports a failed revocation, so an unrevoked credential is never assumed gone', async () => {
    const { provider } = recordingProvider(() => new Response(null, { status: 500 }));
    await assert.rejects(() => provider.revoke('1700000000:device'), /revocation failed \(500\)/);
  });

  it('has a relay-policy-independent STUN discovery configuration', () => {
    assert.deepEqual(new CloudflareTurnProvider(keyId, apiToken).stunServers(), [
      { urls: ['stun:stun.cloudflare.com:3478'] },
    ]);
  });
});
