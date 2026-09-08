/** APNs registration, generic attention delivery, deduplication, and cleanup. */

import assert from 'node:assert/strict';
import { generateKeyPairSync } from 'node:crypto';
import { after, describe, it } from 'node:test';

import { ATTENTION_PAYLOAD, Http2ApnsSender, apnsHost, classifyResponse } from './apns.ts';
import { loadConfig } from './config.ts';
import { FakeApns, enrollPair, startHarness } from './test-harness.ts';

const TOKEN = 'ab'.repeat(32);
const EVENT = '0123456789abcdef0123456789abcdef';

describe('push registration', () => {
  it('accepts one token per controller and refuses hosts and malformed tokens', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const registered = await harness.request('PUT', '/v1/push-registrations', {
      token: client.token, body: { pushToken: TOKEN },
    });
    assert.equal(registered.status, 200);
    assert.deepEqual(registered.body, { registered: true });
    assert.equal((await harness.store.getPushRegistration(client.deviceId))?.pushToken, TOKEN);

    const replaced = await harness.request('PUT', '/v1/push-registrations', {
      token: client.token, body: { pushToken: 'cd'.repeat(32) },
    });
    assert.equal(replaced.status, 200);
    assert.equal((await harness.store.getPushRegistration(client.deviceId))?.pushToken, 'cd'.repeat(32));

    const hostRegistration = await harness.request('PUT', '/v1/push-registrations', {
      token: host.token, body: { pushToken: TOKEN },
    });
    assert.equal(hostRegistration.status, 403);
    const malformed = await harness.request('PUT', '/v1/push-registrations', {
      token: client.token, body: { pushToken: 'not-a-token' },
    });
    assert.equal(malformed.status, 400);
    const smuggled = await harness.request('PUT', '/v1/push-registrations', {
      token: client.token, body: { pushToken: TOKEN, sessionName: 'secret' },
    });
    assert.equal(smuggled.status, 400);
    assert.equal(smuggled.body.field, 'sessionName');

    const removed = await harness.request('DELETE', '/v1/push-registrations', { token: client.token });
    assert.equal(removed.status, 200);
    assert.equal(await harness.store.getPushRegistration(client.deviceId), null);
  });
});

describe('attention delivery', () => {
  it('sends one generic alert per event id, deduplicates, and carries no content', async () => {
    const apns = new FakeApns();
    const harness = await startHarness({}, { apns });
    after(() => harness.close());
    const { host, client } = await enrollPair(harness, 'observe');
    await harness.request('PUT', '/v1/push-registrations', { token: client.token, body: { pushToken: TOKEN } });

    const first = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: EVENT },
    });
    assert.equal(first.status, 202);
    assert.deepEqual(first.body, { eventId: EVENT, delivered: true });
    assert.equal(apns.deliveries.length, 1);
    assert.equal(apns.deliveries[0]!.token, TOKEN);
    assert.equal(apns.deliveries[0]!.collapseId, EVENT);
    assert.equal(apns.deliveries[0]!.expiresAt, harness.nowSeconds() + 600);
    // The delivery input names nothing but the token, an opaque id, and a
    // time; the payload itself is the fixed sentence.
    assert.deepEqual(Object.keys(apns.deliveries[0]!).sort(), ['collapseId', 'expiresAt', 'token']);
    assert.deepEqual(ATTENTION_PAYLOAD, {
      aps: { alert: { title: 'Latch', body: 'Your Mac needs your attention.' }, sound: 'default', 'thread-id': 'latch-attention' },
    });

    const duplicate = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: EVENT },
    });
    assert.equal(duplicate.status, 202);
    assert.deepEqual(duplicate.body, { eventId: EVENT, delivered: false, reason: 'duplicate' });
    assert.equal(apns.deliveries.length, 1);

    const smuggled = await harness.request('POST', '/v1/attention', {
      token: host.token,
      body: { clientDeviceId: client.deviceId, eventId: 'f'.repeat(32), prompt: 'allow rm -rf?' },
    });
    assert.equal(smuggled.status, 400);
    assert.equal(smuggled.body.field, 'prompt');
    assert.equal(apns.deliveries.length, 1);

    const dump = JSON.stringify(harness.store.snapshot());
    assert.equal(dump.includes('prompt'), false);
    assert.equal(dump.includes(TOKEN), true, 'the opaque token is the one thing stored');
  });

  it('refuses unpaired, foreign, or controller callers and is unregistered without a token', async () => {
    const apns = new FakeApns();
    const harness = await startHarness({}, { apns });
    after(() => harness.close());
    const { host, client, accountToken } = await enrollPair(harness);
    const stranger = await harness.request('POST', '/v1/devices', {
      token: accountToken,
      body: { name: 'Other', platform: 'ios', role: 'client', publicKey: 'ef'.repeat(32) },
    });

    const controllerCall = await harness.request('POST', '/v1/attention', {
      token: client.token, body: { clientDeviceId: client.deviceId, eventId: EVENT },
    });
    assert.equal(controllerCall.status, 403);
    const unpaired = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: stranger.body.deviceId, eventId: EVENT },
    });
    assert.equal(unpaired.status, 403);
    const unregistered = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: EVENT },
    });
    assert.deepEqual(unregistered.body, { eventId: EVENT, delivered: false, reason: 'unregistered' });
    assert.equal(apns.deliveries.length, 0);
  });

  it('drops an invalid token and stops after revoke or unpair', async () => {
    const apns = new FakeApns();
    const harness = await startHarness({}, { apns });
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    await harness.request('PUT', '/v1/push-registrations', { token: client.token, body: { pushToken: TOKEN } });
    apns.outcomes.set(TOKEN, 'invalid_token');
    const invalid = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: EVENT },
    });
    assert.deepEqual(invalid.body, { eventId: EVENT, delivered: false, reason: 'invalid_token' });
    assert.equal(await harness.store.getPushRegistration(client.deviceId), null);

    // Registered again, then unpaired: the registration goes with the pairing.
    await harness.request('PUT', '/v1/push-registrations', { token: client.token, body: { pushToken: TOKEN } });
    await harness.request('DELETE', `/v1/pairings/${client.deviceId}`, { token: host.token });
    assert.equal(await harness.store.getPushRegistration(client.deviceId), null);
    const afterUnpair = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: 'a'.repeat(32) },
    });
    assert.equal(afterUnpair.status, 403);

    // Re-paired, registered, then revoked: the same cleanup, and the revoked
    // device can no longer register.
    await harness.request('POST', '/v1/pairings', {
      token: host.token, body: { clientDeviceId: client.deviceId, permission: 'interact' },
    });
    await harness.request('PUT', '/v1/push-registrations', { token: client.token, body: { pushToken: TOKEN } });
    await harness.request('POST', `/v1/devices/${client.deviceId}/revoke`, { token: host.token, body: {} });
    assert.equal(await harness.store.getPushRegistration(client.deviceId), null);
    const revokedRegistration = await harness.request('PUT', '/v1/push-registrations', {
      token: client.token, body: { pushToken: TOKEN },
    });
    assert.equal(revokedRegistration.status, 401);
    assert.equal(apns.deliveries.length, 1);
  });

  it('answers unconfigured without failing when no sender exists and rate-limits hosts', async () => {
    const harness = await startHarness({ ATTENTION_RATE_PER_HOST: '4' });
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    await harness.request('PUT', '/v1/push-registrations', { token: client.token, body: { pushToken: TOKEN } });
    for (let index = 0; index < 4; index += 1) {
      const response = await harness.request('POST', '/v1/attention', {
        token: host.token, body: { clientDeviceId: client.deviceId, eventId: `${index}`.repeat(32) },
      });
      assert.equal(response.status, 202);
      assert.equal(response.body.reason, 'unconfigured');
    }
    const limited = await harness.request('POST', '/v1/attention', {
      token: host.token, body: { clientDeviceId: client.deviceId, eventId: '9'.repeat(32) },
    });
    assert.equal(limited.status, 429);
  });
});

describe('apns sender', () => {
  it('classifies responses and signs a provider token', () => {
    assert.equal(classifyResponse(200, ''), 'delivered');
    assert.equal(classifyResponse(410, '{"reason":"Unregistered"}'), 'invalid_token');
    assert.equal(classifyResponse(400, '{"reason":"BadDeviceToken"}'), 'invalid_token');
    assert.equal(classifyResponse(400, '{"reason":"BadCollapseId"}'), 'failed');
    assert.equal(classifyResponse(503, 'not json'), 'failed');
    assert.equal(apnsHost('sandbox'), 'https://api.sandbox.push.apple.com');
    assert.equal(apnsHost('production'), 'https://api.push.apple.com');

    const key = generateKeyPairSync('ec', { namedCurve: 'P-256' }).privateKey
      .export({ type: 'pkcs8', format: 'pem' }).toString();
    let clock = Date.UTC(2026, 8, 7);
    const sender = new Http2ApnsSender(
      { keyId: 'ABCDE12345', teamId: 'X84RPB4674', privateKeyPem: key, topic: 'dev.cooperativ.latch.mobile', environment: 'sandbox' },
      () => clock,
    );
    const first = sender.bearer();
    const [header, claims] = first.split('.');
    assert.deepEqual(JSON.parse(Buffer.from(header!, 'base64url').toString()), { alg: 'ES256', kid: 'ABCDE12345' });
    assert.deepEqual(JSON.parse(Buffer.from(claims!, 'base64url').toString()), { iss: 'X84RPB4674', iat: Math.floor(clock / 1000) });
    clock += 10 * 60 * 1000;
    assert.equal(sender.bearer(), first, 'reused inside the validity window');
    clock += 50 * 60 * 1000;
    assert.notEqual(sender.bearer(), first, 'refreshed before Apple rejects it');
  });

  it('configures all-or-nothing from the environment', () => {
    const base = { DATABASE_URL: 'postgres://unused/test' };
    assert.equal(loadConfig(base).apns, null);
    assert.throws(() => loadConfig({ ...base, APNS_KEY_ID: 'ABCDE12345' }), /together/);
    assert.throws(() => loadConfig({ ...base, APNS_ENVIRONMENT: 'staging' }), /sandbox or production/);
    const configured = loadConfig({
      ...base, APNS_KEY_ID: 'ABCDE12345', APNS_TEAM_ID: 'X84RPB4674',
      APNS_PRIVATE_KEY_PEM: '-----BEGIN PRIVATE KEY-----\\nabc\\n-----END PRIVATE KEY-----',
      APNS_ENVIRONMENT: 'production', APNS_TOPIC: 'dev.cooperativ.latch.mobile',
    });
    assert.equal(configured.apns?.environment, 'production');
    assert.equal(configured.apns?.privateKeyPem.includes('\n'), true);
  });
});
