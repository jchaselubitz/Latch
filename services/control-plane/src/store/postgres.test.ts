/**
 * PostgreSQL contract test.
 *
 * Skipped unless TEST_DATABASE_URL points at a throwaway database: it applies
 * the real migrations and exercises the same behaviours the in-memory store
 * provides, so the two implementations cannot drift silently.
 */

import assert from 'node:assert/strict';
import { after, before, describe, it } from 'node:test';

import { loadMigrations, runMigrations } from '../migrate.ts';
import { PostgresStore } from './postgres.ts';

const connectionString = process.env.TEST_DATABASE_URL;

describe('postgres store', { skip: connectionString ? false : 'TEST_DATABASE_URL is not set' }, () => {
  let store: PostgresStore;
  const now = Math.floor(Date.UTC(2026, 0, 1) / 1000);

  before(async () => {
    store = new PostgresStore({
      connectionString: connectionString!,
      poolSize: 2,
      sslRejectUnauthorized: false,
    });
    // Each run starts from an empty schema so assertions are deterministic.
    await store.pool.query('DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public');
    await runMigrations(store.pool, await loadMigrations());
  });

  after(async () => {
    await store?.close();
  });

  it('round-trips accounts, devices, pairings, presence, offers, and tickets', async () => {
    const account = await store.createAccount({
      id: 'acct_' + 'a'.repeat(32),
      label: 'Integration',
      tokenDigest: 'digest-account',
    });
    assert.equal(account.relayEnabled, true);
    assert.equal((await store.accountByTokenDigest(account.id, 'digest-account'))?.id, account.id);
    assert.equal(await store.accountByTokenDigest(account.id, 'wrong'), null);

    const host = await store.createDevice({
      id: 'dev_' + 'b'.repeat(32),
      accountId: account.id,
      name: 'Mac',
      platform: 'macos',
      role: 'host',
      publicKey: 'ab'.repeat(32),
      tokenDigest: 'digest-host',
    });
    const client = await store.createDevice({
      id: 'dev_' + 'c'.repeat(32),
      accountId: account.id,
      name: 'Phone',
      platform: 'ios',
      role: 'client',
      publicKey: 'cd'.repeat(32),
      tokenDigest: 'digest-client',
    });
    assert.equal(await store.countDevices(account.id), 2);
    assert.equal((await store.deviceByTokenDigest(host.id, 'digest-host'))?.id, host.id);

    const pairing = await store.upsertPairing(account.id, host.id, client.id, 'interact');
    assert.equal(pairing.permission, 'interact');
    // Upsert is idempotent and updates the permission in place.
    assert.equal(
      (await store.upsertPairing(account.id, host.id, client.id, 'control')).permission,
      'control',
    );
    assert.equal((await store.listPairingsForDevice(client.id)).length, 1);

    await store.publishPresence({
      deviceId: host.id,
      accountId: account.id,
      candidates: [{ address: '198.51.100.4:52111', expiresAt: now + 90 }],
      iceUfrag: 'hostUfrag_123',
      icePwd: 'hostPassword_1234567890',
      expiresAt: now + 90,
    });
    assert.equal((await store.getPresence(host.id, now))?.candidates[0]?.address, '198.51.100.4:52111');
    assert.equal((await store.getPresence(host.id, now))?.iceUfrag, 'hostUfrag_123');
    assert.equal((await store.getPresence(host.id, now))?.icePwd, 'hostPassword_1234567890');
    // An expired row is never served even before the sweeper removes it.
    assert.equal(await store.getPresence(host.id, now + 91), null);

    await store.createOffer({
      id: 'rdv_' + 'd'.repeat(32),
      accountId: account.id,
      requesterDeviceId: client.id,
      targetDeviceId: host.id,
      requestId: 'request-0001',
      candidates: [{ address: '203.0.113.9:41000', expiresAt: now + 60 }],
      iceUfrag: 'clientUfrag_123',
      icePwd: 'clientPassword_12345678',
      expiresAt: now + 60,
    });
    const offers = await store.takeOffers(host.id, now);
    assert.equal(offers.length, 1);
    assert.equal(offers[0]?.iceUfrag, 'clientUfrag_123');
    assert.equal(offers[0]?.icePwd, 'clientPassword_12345678');
    assert.equal((await store.takeOffers(host.id, now)).length, 0);

    const ticket = await store.createRelayTicket({
      relayId: 'e'.repeat(32),
      accountId: account.id,
      hostDeviceId: host.id,
      clientDeviceId: client.id,
      secretDigest: 'digest-ticket',
      expiresAt: now + 60,
    });
    assert.equal(await store.admitToRelayTicket(ticket.relayId, host.id, now), true);
    assert.equal(await store.admitToRelayTicket(ticket.relayId, host.id, now), false);
    assert.equal(await store.admitToRelayTicket(ticket.relayId, client.id, now), true);
    assert.equal(await store.admitToRelayTicket(ticket.relayId, client.id, now + 61), null);

    await store.recordAccessEvent({
      accountId: account.id,
      deviceId: client.id,
      action: 'relay.admit',
      result: 'allowed',
      createdAt: new Date(now * 1000).toISOString(),
    });
    assert.equal((await store.listAccessEvents(account.id, 10))[0]?.action, 'relay.admit');

    // Revocation clears presence, offers, tickets, and pairings in one step.
    await store.publishPresence({
      deviceId: client.id,
      accountId: account.id,
      candidates: [{ address: '198.51.100.5:52112', expiresAt: now + 90 }],
      expiresAt: now + 90,
    });
    const revoked = await store.revokeDevice(client.id, new Date(now * 1000).toISOString());
    assert.notEqual(revoked?.revokedAt, null);
    assert.equal(await store.deviceByTokenDigest(client.id, 'digest-client'), null);
    assert.equal(await store.getPresence(client.id, now), null);
    assert.equal(await store.getPairing(host.id, client.id), null);
    assert.equal(await store.getRelayTicket(ticket.relayId, now), null);

    // Pairing requests are single-use and expiry-filtered in SQL.
    const pairingRequest = await store.createPairingRequest({
      pairingId: 'f'.repeat(32),
      accountId: account.id,
      hostDeviceId: host.id,
      secretDigest: 'digest-pairing',
      phrase: 'anchor-basin-cedar-dune-ember-flint',
      permission: 'interact',
      expiresAt: now + 300,
    });
    assert.equal((await store.getPairingRequest(pairingRequest.pairingId, now))?.phrase, pairingRequest.phrase);
    assert.equal(await store.countPendingPairingRequests(host.id, now), 1);
    const consumedAt = new Date(now * 1000).toISOString();
    assert.equal(await store.consumePairingRequest(pairingRequest.pairingId, consumedAt, now), true);
    assert.equal(await store.consumePairingRequest(pairingRequest.pairingId, consumedAt, now), false);
    assert.equal(await store.getPairingRequest(pairingRequest.pairingId, now), null);

    assert.equal(typeof (await store.purgeExpired(now + 1_000)), 'number');
  });
});
