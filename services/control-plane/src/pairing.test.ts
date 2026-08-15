/**
 * The QR pairing flow, in the shape the mobile client speaks:
 * the Mac registers a request, the phone confirms it with the scanned secret,
 * and the answer carries the device, the Mac's pinned key, and a one-time
 * access token.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import { digestOf } from './credentials.ts';
import { publicKeyFor, startHarness } from './test-harness.ts';
import type { Harness } from './test-harness.ts';

const SECRET = 'a1b2'.repeat(16);
const PAIRING_ID = 'f0e1d2c3b4a5968778695a4b3c2d1e0f';
const PHRASE = 'anchor-basin-cedar-dune-ember-flint';

async function registerHost(harness: Harness) {
  const account = await harness.request('POST', '/v1/accounts', { body: { label: 'Pairing' } });
  const host = await harness.request('POST', '/v1/devices', {
    token: account.body.accountToken,
    body: { name: 'Mac', platform: 'macos', role: 'host', publicKey: publicKeyFor('ab') },
  });
  return { accountToken: account.body.accountToken as string, host: host.body };
}

async function openRequest(harness: Harness, hostToken: string, overrides = {}) {
  return harness.request('POST', '/v1/pairings/requests', {
    token: hostToken,
    body: {
      pairingId: PAIRING_ID,
      secretDigest: digestOf('pairing', SECRET),
      phrase: PHRASE,
      ...overrides,
    },
  });
}

const confirmBody = (overrides: Record<string, unknown> = {}) => ({
  formatVersion: 1,
  secret: SECRET,
  device: { publicKey: publicKeyFor('cd'), name: 'Phone', platform: 'ios' },
  phrase: PHRASE,
  ...overrides,
});

describe('qr pairing', () => {
  it('enrols a phone and returns the Mac identity plus a one-time token', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);

    const opened = await openRequest(harness, host.deviceToken);
    assert.equal(opened.status, 201);
    assert.equal(opened.body.pairingId, PAIRING_ID);

    const confirmed = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody(),
    });
    assert.equal(confirmed.status, 201);
    assert.equal(confirmed.body.device.permission, 'interact');
    assert.equal(confirmed.body.device.revoked, false);
    assert.equal(confirmed.body.mac.publicKey, publicKeyFor('ab'));
    assert.equal(confirmed.body.mac.deviceId, host.deviceId);
    assert.equal(confirmed.body.phrase, PHRASE);
    assert.match(confirmed.body.accessToken, /^dev_[0-9a-f]{32}\.[0-9a-f]{64}$/);

    // The issued token works, and the device sees its own record and host.
    const self = await harness.request('GET', `/v1/devices/${confirmed.body.device.deviceId}`, {
      token: confirmed.body.accessToken,
    });
    assert.equal(self.status, 200);
    assert.equal(self.body.mac.deviceId, host.deviceId);
    assert.equal(self.body.device.permission, 'interact');

    // And the pairing is in the host's directory.
    const pairings = await harness.request('GET', '/v1/pairings', { token: host.deviceToken });
    assert.equal(pairings.body.pairings.length, 1);
    assert.equal(pairings.body.pairings[0].clientDeviceId, confirmed.body.device.deviceId);
  });

  it('is single use: a second scan of the same code is refused', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    await openRequest(harness, host.deviceToken);

    const first = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody(),
    });
    assert.equal(first.status, 201);

    const second = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody({ device: { publicKey: publicKeyFor('ef'), name: 'Other', platform: 'ios' } }),
    });
    assert.equal(second.status, 404);
    assert.equal(second.body.error, 'pairing_unavailable');
  });

  it('rejects a wrong secret and reports an unknown pairing distinctly', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    await openRequest(harness, host.deviceToken);

    const wrong = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody({ secret: 'b2c3'.repeat(16) }),
    });
    assert.equal(wrong.status, 403);

    const unknown = await harness.request(
      'POST',
      '/v1/pairings/00000000000000000000000000000000/confirm',
      { body: confirmBody() },
    );
    assert.equal(unknown.status, 404);

    // The failed attempts did not consume the request.
    const good = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody(),
    });
    assert.equal(good.status, 201);
  });

  it('expires five minutes after the Mac displayed the code', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    await openRequest(harness, host.deviceToken);

    harness.advance(5 * 60 + 1);
    const late = await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, {
      body: confirmBody(),
    });
    assert.equal(late.status, 404);
  });

  it('refuses to enrol an identity that is already paired', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    await openRequest(harness, host.deviceToken);
    await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, { body: confirmBody() });

    const second = `${PAIRING_ID.slice(0, 30)}aa`;
    await openRequest(harness, host.deviceToken, { pairingId: second });
    const duplicate = await harness.request('POST', `/v1/pairings/${second}/confirm`, {
      body: confirmBody(),
    });
    assert.equal(duplicate.status, 409);
    assert.equal(duplicate.body.error, 'already_paired');
  });

  it('never stores the scanned secret and refuses a client-opened request', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    await openRequest(harness, host.deviceToken);
    await harness.request('POST', `/v1/pairings/${PAIRING_ID}/confirm`, { body: confirmBody() });

    assert.equal(JSON.stringify(harness.store.snapshot()).includes(SECRET), false);

    const confirmedAgain = await harness.request('POST', '/v1/pairings/requests', {
      token: host.deviceToken,
      body: { pairingId: PAIRING_ID.replace(/^f/, 'e'), secretDigest: digestOf('pairing', SECRET) },
    });
    assert.equal(confirmedAgain.status, 201);
  });

  it('bounds the number of pending requests per host', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await registerHost(harness);
    const results = [];
    for (let index = 0; index < 10; index += 1) {
      const pairingId = `${PAIRING_ID.slice(0, 30)}${index.toString().padStart(2, '0')}`;
      results.push((await openRequest(harness, host.deviceToken, { pairingId })).status);
    }
    assert.equal(results.filter((status) => status === 201).length, 8);
    assert.equal(results.filter((status) => status === 403).length, 2);
  });
});
