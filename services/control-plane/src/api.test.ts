/**
 * End-to-end coverage of the control-plane API over the real HTTP surface.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import {
  candidate,
  enrollPair,
  publicKeyFor,
  RELAY_SERVICE_TOKEN,
  startHarness,
} from './test-harness.ts';

describe('health', () => {
  it('reports live, ready, and migration count', async () => {
    const harness = await startHarness();
    after(() => harness.close());

    const live = await harness.request('GET', '/health/live');
    assert.equal(live.status, 200);
    assert.equal(live.body.status, 'live');

    const ready = await harness.request('GET', '/health/ready');
    assert.equal(ready.status, 200);
    assert.equal(ready.body.migrations, 1);
    assert.equal(ready.body.relayConfigured, true);
  });

  it('reports not ready when storage fails', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    harness.store.ping = async () => {
      throw new Error('down');
    };
    const ready = await harness.request('GET', '/health/ready');
    assert.equal(ready.status, 503);
    assert.equal(ready.body.error, 'not_ready');
  });
});

describe('registration', () => {
  it('registers an account and devices, issuing each credential once', async () => {
    const harness = await startHarness();
    after(() => harness.close());

    const account = await harness.request('POST', '/v1/accounts', { body: { label: 'Jake' } });
    assert.equal(account.status, 201);
    assert.match(account.body.accountToken, /^acct_[0-9a-f]{32}\.[0-9a-f]{64}$/);

    const device = await harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name: 'Studio Mac', platform: 'macos', role: 'host', publicKey: publicKeyFor('ab') },
    });
    assert.equal(device.status, 201);
    assert.equal(device.body.role, 'host');
    assert.equal(device.body.revoked, false);

    // The device credential is never echoed by a later read.
    const listed = await harness.request('GET', '/v1/devices', { token: device.body.deviceToken });
    assert.equal(listed.status, 200);
    assert.equal(listed.body.devices.length, 1);
    assert.equal('deviceToken' in listed.body.devices[0], false);
  });

  it('rejects registration without a valid account credential', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const anonymous = await harness.request('POST', '/v1/devices', {
      body: { name: 'Rogue', platform: 'ios', role: 'client', publicKey: publicKeyFor('ab') },
    });
    assert.equal(anonymous.status, 401);
  });

  it('enforces the per-account device limit', async () => {
    const harness = await startHarness({ MAX_DEVICES_PER_ACCOUNT: '2' });
    after(() => harness.close());
    const account = await harness.request('POST', '/v1/accounts', { body: {} });
    const register = (name: string) =>
      harness.request('POST', '/v1/devices', {
        token: account.body.accountToken,
        body: { name, platform: 'ios', role: 'client', publicKey: publicKeyFor('cd') },
      });
    assert.equal((await register('One')).status, 201);
    assert.equal((await register('Two')).status, 201);
    assert.equal((await register('Three')).status, 403);
  });
});

describe('pairing directory', () => {
  it('lists only paired devices and the caller itself', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);

    const unpaired = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Other', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });

    const directory = await harness.request('GET', '/v1/devices', { token: client.token });
    const ids = directory.body.devices.map((device: { deviceId: string }) => device.deviceId);
    assert.deepEqual(ids.sort(), [host.deviceId, client.deviceId].sort());
    assert.equal(ids.includes(unpaired.body.deviceId), false);

    const self = directory.body.devices.find(
      (device: { deviceId: string }) => device.deviceId === client.deviceId,
    );
    assert.equal(self.self, true);
    const peer = directory.body.devices.find(
      (device: { deviceId: string }) => device.deviceId === host.deviceId,
    );
    assert.equal(peer.permission, 'interact');
  });

  it('refuses to let a client device declare a pairing', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const attempt = await harness.request('POST', '/v1/pairings', {
      token: client.token,
      body: { clientDeviceId: host.deviceId },
    });
    assert.equal(attempt.status, 403);
  });

  it('unpairs from either side', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const removed = await harness.request('DELETE', `/v1/pairings/${host.deviceId}`, {
      token: client.token,
    });
    assert.equal(removed.status, 200);
    const pairings = await harness.request('GET', '/v1/pairings', { token: host.token });
    assert.deepEqual(pairings.body.pairings, []);
  });
});

describe('presence and rendezvous', () => {
  it('exchanges candidates between paired devices', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);

    const published = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness, '198.51.100.4:52111')] },
    });
    assert.equal(published.status, 200);
    assert.equal(published.body.ttlSeconds <= harness.config.presenceTtlSeconds, true);

    const rendezvous = await harness.request('POST', '/v1/rendezvous', {
      token: client.token,
      body: {
        targetDeviceId: host.deviceId,
        requestId: 'request-0001',
        candidates: [candidate(harness, '[2001:db8::1]:41999')],
      },
    });
    assert.equal(rendezvous.status, 200);
    assert.equal(rendezvous.body.peerDeviceId, host.deviceId);
    assert.equal(rendezvous.body.peerIdentityKey, publicKeyFor('ab'));
    assert.equal(rendezvous.body.candidates[0].address, '198.51.100.4:52111');

    // The host collects the offer exactly once.
    const offers = await harness.request('GET', '/v1/rendezvous', { token: host.token });
    assert.equal(offers.body.offers.length, 1);
    assert.equal(offers.body.offers[0].requestId, 'request-0001');
    assert.equal(offers.body.offers[0].peerDeviceId, client.deviceId);
    const again = await harness.request('GET', '/v1/rendezvous', { token: host.token });
    assert.deepEqual(again.body.offers, []);
  });

  it('expires presence after its short lifetime', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness)] },
    });

    harness.advance(harness.config.presenceTtlSeconds + 1);
    const presence = await harness.request('GET', `/v1/presence/${host.deviceId}`, {
      token: client.token,
    });
    assert.equal(presence.body.online, false);

    const rendezvous = await harness.request('POST', '/v1/rendezvous', {
      token: client.token,
      body: {
        targetDeviceId: host.deviceId,
        requestId: 'request-0002',
        candidates: [candidate(harness)],
      },
    });
    assert.equal(rendezvous.status, 409);
    assert.equal(rendezvous.body.error, 'target_offline');
  });

  it('refuses presence and rendezvous between unpaired devices', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const stranger = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Stranger', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });
    await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness)] },
    });

    const peek = await harness.request('GET', `/v1/presence/${host.deviceId}`, {
      token: stranger.body.deviceToken,
    });
    assert.equal(peek.status, 403);

    const rendezvous = await harness.request('POST', '/v1/rendezvous', {
      token: stranger.body.deviceToken,
      body: {
        targetDeviceId: host.deviceId,
        requestId: 'request-0003',
        candidates: [candidate(harness)],
      },
    });
    assert.equal(rendezvous.status, 403);
    assert.equal(client.deviceId.startsWith('dev_'), true);
  });

  it('rejects hostname and over-long candidates', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await enrollPair(harness);

    const hostname = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [{ address: 'gateway.internal:8080', expiresAt: harness.nowSeconds() + 30 }] },
    });
    assert.equal(hostname.status, 400);

    const tooLong = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness, '203.0.113.9:443', 4000)] },
    });
    assert.equal(tooLong.status, 400);
  });
});

describe('relay tickets', () => {
  it('issues a ticket and authorizes each endpoint once', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);

    const ticket = await harness.request('POST', '/v1/relay-tickets', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });
    assert.equal(ticket.status, 201);
    assert.match(ticket.body.relayId, /^[0-9a-f]{32}$/);
    assert.match(ticket.body.authenticationSecret, /^[0-9a-f]{64}$/);
    assert.equal(ticket.body.relayUrl, 'wss://relay.latch.test');
    assert.equal(ticket.body.expiresAt - harness.nowSeconds(), harness.config.relayTicketTtlSeconds);

    const admit = (deviceId: string) =>
      harness.request('POST', '/v1/relay-tickets/authorize', {
        token: RELAY_SERVICE_TOKEN,
        body: {
          relayId: ticket.body.relayId,
          deviceId,
          authenticationSecret: ticket.body.authenticationSecret,
        },
      });

    const first = await admit(client.deviceId);
    assert.equal(first.status, 200);
    assert.equal(first.body.peerDeviceId, host.deviceId);

    const second = await admit(host.deviceId);
    assert.equal(second.status, 200);

    const duplicate = await admit(client.deviceId);
    assert.equal(duplicate.status, 409);
  });

  it('rejects the relay call without the relay service credential', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const ticket = await harness.request('POST', '/v1/relay-tickets', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });
    const unauthenticated = await harness.request('POST', '/v1/relay-tickets/authorize', {
      token: client.token,
      body: {
        relayId: ticket.body.relayId,
        deviceId: client.deviceId,
        authenticationSecret: ticket.body.authenticationSecret,
      },
    });
    assert.equal(unauthenticated.status, 401);
  });

  it('rejects a wrong secret, an expired ticket, and a third device', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const ticket = await harness.request('POST', '/v1/relay-tickets', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });

    const wrongSecret = await harness.request('POST', '/v1/relay-tickets/authorize', {
      token: RELAY_SERVICE_TOKEN,
      body: {
        relayId: ticket.body.relayId,
        deviceId: client.deviceId,
        authenticationSecret: 'f'.repeat(64),
      },
    });
    assert.equal(wrongSecret.status, 403);

    const stranger = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Stranger', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });
    const wrongDevice = await harness.request('POST', '/v1/relay-tickets/authorize', {
      token: RELAY_SERVICE_TOKEN,
      body: {
        relayId: ticket.body.relayId,
        deviceId: stranger.body.deviceId,
        authenticationSecret: ticket.body.authenticationSecret,
      },
    });
    assert.equal(wrongDevice.status, 403);

    harness.advance(harness.config.relayTicketTtlSeconds + 1);
    const expired = await harness.request('POST', '/v1/relay-tickets/authorize', {
      token: RELAY_SERVICE_TOKEN,
      body: {
        relayId: ticket.body.relayId,
        deviceId: client.deviceId,
        authenticationSecret: ticket.body.authenticationSecret,
      },
    });
    assert.equal(expired.status, 403);
  });

  it('honours the account relay switch', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const disabled = await harness.request('PATCH', '/v1/account', {
      token: accountToken,
      body: { relayEnabled: false },
    });
    assert.equal(disabled.body.relayEnabled, false);

    const ticket = await harness.request('POST', '/v1/relay-tickets', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });
    assert.equal(ticket.status, 403);
  });
});

describe('revocation', () => {
  it('immediately ends authentication, presence, pairing, and relay admission', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    await harness.request('POST', '/v1/presence', {
      token: client.token,
      body: { candidates: [candidate(harness)] },
    });
    const ticket = await harness.request('POST', '/v1/relay-tickets', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });

    const revoked = await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, {
      token: accountToken,
    });
    assert.equal(revoked.status, 200);
    assert.equal(revoked.body.revoked, true);

    // The revoked device can no longer authenticate at all.
    const afterRevocation = await harness.request('GET', '/v1/devices', { token: client.token });
    assert.equal(afterRevocation.status, 401);

    // Its unexpired relay ticket no longer admits anyone.
    const admit = await harness.request('POST', '/v1/relay-tickets/authorize', {
      token: RELAY_SERVICE_TOKEN,
      body: {
        relayId: ticket.body.relayId,
        deviceId: host.deviceId,
        authenticationSecret: ticket.body.authenticationSecret,
      },
    });
    assert.equal(admit.status, 403);

    // And the pairing is gone from the host's directory.
    const pairings = await harness.request('GET', '/v1/pairings', { token: host.token });
    assert.deepEqual(pairings.body.pairings, []);
  });

  it('lets a host revoke a paired client but not an unrelated device', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const stranger = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Stranger', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });

    const unrelated = await harness.request(
      'POST',
      `/v1/devices/${stranger.body.deviceId}/revoke`,
      { token: host.token },
    );
    assert.equal(unrelated.status, 403);

    const paired = await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, {
      token: host.token,
    });
    assert.equal(paired.status, 200);
  });

  it('refuses key rotation for a revoked device', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, client } = await enrollPair(harness);
    await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, { token: accountToken });
    const rotate = await harness.request('POST', `/v1/devices/${client.deviceId}/rotate-key`, {
      token: client.token,
      body: { publicKey: publicKeyFor('ef') },
    });
    assert.equal(rotate.status, 401);
  });

  it('rotates a device key in place', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { client } = await enrollPair(harness);
    const rotated = await harness.request('POST', `/v1/devices/${client.deviceId}/rotate-key`, {
      token: client.token,
      body: { publicKey: publicKeyFor('ef') },
    });
    assert.equal(rotated.status, 200);
    assert.equal(rotated.body.publicKey, publicKeyFor('ef'));
    assert.equal(rotated.body.keyGeneration, 2);
  });
});

describe('audit trail', () => {
  it('records coarse, content-free access events', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness)] },
    });
    await harness.request('POST', '/v1/rendezvous', {
      token: client.token,
      body: {
        targetDeviceId: host.deviceId,
        requestId: 'request-0004',
        candidates: [candidate(harness)],
      },
    });

    const events = await harness.request('GET', '/v1/account/events', { token: accountToken });
    const actions = events.body.events.map((event: { action: string }) => event.action);
    assert.equal(actions.includes('rendezvous.request'), true);
    for (const event of events.body.events) {
      assert.deepEqual(Object.keys(event).sort(), [
        'accountId',
        'action',
        'createdAt',
        'deviceId',
        'result',
      ]);
    }
  });
});

describe('request handling', () => {
  it('rejects unknown routes, methods, media types, and oversized bodies', async () => {
    const harness = await startHarness();
    after(() => harness.close());

    assert.equal((await harness.request('GET', '/nope')).status, 404);
    assert.equal((await harness.request('DELETE', '/health/live')).status, 405);

    const wrongType = await fetch(`${harness.baseUrl}/v1/accounts`, {
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: 'label=x',
    });
    assert.equal(wrongType.status, 415);

    const huge = await fetch(`${harness.baseUrl}/v1/accounts`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ label: 'x'.repeat(64 * 1024) }),
    });
    assert.equal(huge.status, 413);
  });

  it('rate limits a noisy device', async () => {
    const harness = await startHarness({ RATE_LIMIT_PER_MINUTE: '10' });
    after(() => harness.close());
    const { host } = await enrollPair(harness);
    let limited = 0;
    for (let index = 0; index < 20; index += 1) {
      const response = await harness.request('GET', '/v1/pairings', { token: host.token });
      if (response.status === 429) {
        limited += 1;
      }
    }
    assert.equal(limited > 0, true);
  });
});
