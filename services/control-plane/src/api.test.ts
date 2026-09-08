/** End-to-end coverage of the supported control-plane API over HTTP. */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import {
  createOwnerAccount,
  enrollPair,
  publicKeyFor,
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
    harness.store.ping = async () => { throw new Error('down'); };
    const ready = await harness.request('GET', '/health/ready');
    assert.equal(ready.status, 503);
    assert.equal(ready.body.error, 'not_ready');
  });
});

describe('registration and grant directory', () => {
  it('issues credentials once and exposes only paired devices', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const account = await createOwnerAccount(harness, 'Jake');
    assert.equal(account.status, 201);
    assert.match(account.body.accountToken, /^acct_[0-9a-f]{32}\.[0-9a-f]{64}$/);
    const host = await harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name: 'Studio Mac', platform: 'macos', role: 'host', publicKey: publicKeyFor('ab') },
    });
    const client = await harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name: 'Phone', platform: 'ios', role: 'client', publicKey: publicKeyFor('cd') },
    });
    const hidden = await harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name: 'Other', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });
    await harness.request('POST', '/v1/pairings', {
      token: host.body.deviceToken,
      body: { clientDeviceId: client.body.deviceId, permission: 'interact' },
    });
    const listed = await harness.request('GET', '/v1/devices', { token: client.body.deviceToken });
    const ids = listed.body.devices.map((device: { deviceId: string }) => device.deviceId);
    assert.deepEqual(ids.sort(), [host.body.deviceId, client.body.deviceId].sort());
    assert.equal(ids.includes(hidden.body.deviceId), false);
    assert.equal('deviceToken' in listed.body.devices[0], false);
    assert.equal('online' in listed.body.devices[0], false);
  });

  it('rejects unauthenticated registration and enforces the device limit', async () => {
    const harness = await startHarness({ MAX_DEVICES_PER_ACCOUNT: '2' });
    after(() => harness.close());
    const anonymous = await harness.request('POST', '/v1/devices', {
      body: { name: 'Rogue', platform: 'ios', role: 'client', publicKey: publicKeyFor('ab') },
    });
    assert.equal(anonymous.status, 401);
    const account = await createOwnerAccount(harness);
    const register = (name: string) => harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name, platform: 'ios', role: 'client', publicKey: publicKeyFor('cd') },
    });
    assert.equal((await register('One')).status, 201);
    assert.equal((await register('Two')).status, 201);
    assert.equal((await register('Three')).status, 403);
  });

  it('lets only a host declare a grant and either side revoke it', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const refused = await harness.request('POST', '/v1/pairings', {
      token: client.token,
      body: { clientDeviceId: host.deviceId },
    });
    assert.equal(refused.status, 403);
    const removed = await harness.request('DELETE', `/v1/pairings/${host.deviceId}`, {
      token: client.token,
    });
    assert.equal(removed.status, 200);
    assert.deepEqual((await harness.request('GET', '/v1/pairings', { token: host.token })).body.pairings, []);
  });
});

describe('revocation and audit', () => {
  it('immediately ends authentication and invalidates the grant', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const revoked = await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, {
      token: accountToken,
    });
    assert.equal(revoked.status, 200);
    assert.equal(revoked.body.revoked, true);
    assert.equal((await harness.request('GET', '/v1/devices', { token: client.token })).status, 401);
    assert.deepEqual((await harness.request('GET', '/v1/pairings', { token: host.token })).body.pairings, []);
  });

  it('lets a host revoke only a paired client and refuses rotation after revocation', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const stranger = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Stranger', platform: 'ios', role: 'client', publicKey: publicKeyFor('ef') },
    });
    assert.equal((await harness.request('POST', `/v1/devices/${stranger.body.deviceId}/revoke`, { token: host.token })).status, 403);
    assert.equal((await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, { token: host.token })).status, 200);
    const rotate = await harness.request('POST', `/v1/devices/${client.deviceId}/rotate-key`, {
      token: client.token,
      body: { publicKey: publicKeyFor('ef') },
    });
    assert.equal(rotate.status, 401);
  });

  it('records only coarse access events', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, client } = await enrollPair(harness);
    await harness.request('GET', '/v1/remote-links', { token: client.token });
    const events = await harness.request('GET', '/v1/account/events', { token: accountToken });
    assert.equal(events.status, 200);
    for (const event of events.body.events) {
      assert.deepEqual(Object.keys(event).sort(), ['accountId', 'action', 'createdAt', 'deviceId', 'result']);
    }
  });
});

describe('request handling', () => {
  it('rejects retired signaling routes and malformed requests', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    for (const path of ['/v1/presence', '/v1/rendezvous', '/v1/ice-servers', '/v1/turn-credentials', '/v1/relay-tickets']) {
      assert.equal((await harness.request('GET', path)).status, 404);
    }
    assert.equal((await harness.request('GET', '/nope')).status, 404);
    assert.equal((await harness.request('DELETE', '/health/live')).status, 405);
    const wrongType = await fetch(`${harness.baseUrl}/v1/accounts/claim`, {
      method: 'POST', headers: { 'content-type': 'text/plain' }, body: 'label=x',
    });
    assert.equal(wrongType.status, 415);
  });

  it('rate limits a noisy device', async () => {
    const harness = await startHarness({ RATE_LIMIT_PER_MINUTE: '10' });
    after(() => harness.close());
    const { host } = await enrollPair(harness);
    let limited = 0;
    for (let index = 0; index < 20; index += 1) {
      if ((await harness.request('GET', '/v1/pairings', { token: host.token })).status === 429) limited += 1;
    }
    assert.equal(limited > 0, true);
  });
});
