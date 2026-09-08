/** PostgreSQL store contract against an explicitly disposable database. */

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
      poolSize: 4,
      sslRejectUnauthorized: false,
    });
    await store.pool.query('DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public');
    await runMigrations(store.pool, await loadMigrations());
  });

  after(async () => { await store?.close(); });

  it('round-trips accounts, devices, grants, revision changes, and revocation', async () => {
    const account = await store.createAccount({
      id: 'acct_' + 'a'.repeat(32), label: 'Integration', tokenDigest: 'digest-account',
    });
    const host = await store.createDevice({
      id: 'dev_' + 'b'.repeat(32), accountId: account.id, name: 'Mac', platform: 'macos',
      role: 'host', publicKey: 'ab'.repeat(32), tokenDigest: 'digest-host',
    });
    const client = await store.createDevice({
      id: 'dev_' + 'c'.repeat(32), accountId: account.id, name: 'Phone', platform: 'ios',
      role: 'client', publicKey: 'cd'.repeat(32), tokenDigest: 'digest-client',
    });
    assert.equal((await store.accountByTokenDigest(account.id, 'digest-account'))?.id, account.id);
    assert.equal((await store.deviceByTokenDigest(host.id, 'digest-host'))?.id, host.id);
    assert.equal((await store.upsertPairing(account.id, host.id, client.id, 'interact')).permission, 'interact');
    const link = await store.getOrCreateRemoteLink(account.id, host.id, client.id, 'A'.repeat(43));
    assert.equal(link.grantRevision, 1);
    await store.upsertPairing(account.id, host.id, client.id, 'observe');
    assert.equal((await store.getRemoteLink(link.id))?.grantRevision, 2);
    await store.recordAccessEvent({
      accountId: account.id, deviceId: client.id, action: 'remote_link.admit',
      result: 'allowed', createdAt: new Date(now * 1000).toISOString(),
    });
    assert.equal((await store.listAccessEvents(account.id, 10))[0]?.action, 'remote_link.admit');
    const revoked = await store.revokeDevice(client.id, new Date(now * 1000).toISOString());
    assert.notEqual(revoked?.revokedAt, null);
    assert.equal(await store.deviceByTokenDigest(client.id, 'digest-client'), null);
    assert.equal(await store.getPairing(host.id, client.id), null);
    assert.equal((await store.listPendingRelayRevocations(10)).some((item) => item.roomId === link.roomId), true);
  });

  it('keeps one push token per device, dedupes attention, and clears on revoke and unpair', async () => {
    const account = await store.createAccount({
      id: 'acct_' + 'e'.repeat(32), label: 'Push', tokenDigest: 'push-account-digest',
    });
    const host = await store.createDevice({
      id: 'dev_' + 'e'.repeat(32), accountId: account.id, name: 'Mac', platform: 'macos',
      role: 'host', publicKey: 'e1'.repeat(32), tokenDigest: 'push-host-digest',
    });
    const client = await store.createDevice({
      id: 'dev_' + 'f'.repeat(32), accountId: account.id, name: 'Phone', platform: 'ios',
      role: 'client', publicKey: 'f1'.repeat(32), tokenDigest: 'push-client-digest',
    });
    await store.upsertPairing(account.id, host.id, client.id, 'interact');
    const at = new Date(now * 1000).toISOString();
    await store.upsertPushRegistration(client.id, 'ab'.repeat(32), at);
    await store.upsertPushRegistration(client.id, 'cd'.repeat(32), at);
    assert.equal((await store.getPushRegistration(client.id))?.pushToken, 'cd'.repeat(32));
    const recorded = await Promise.all([
      store.recordAttentionEvent(host.id, client.id, '1'.repeat(32), at),
      store.recordAttentionEvent(host.id, client.id, '1'.repeat(32), at),
    ]);
    assert.deepEqual(recorded.sort(), [false, true]);
    assert.equal(await store.recordAttentionEvent(host.id, client.id, '2'.repeat(32), at), true);
    assert.equal(await store.purgeExpired(now + 25 * 60 * 60) >= 2, true);
    assert.equal(await store.recordAttentionEvent(host.id, client.id, '1'.repeat(32), at), true);

    assert.equal(await store.revokePairing(host.id, client.id, at), true);
    assert.equal(await store.getPushRegistration(client.id), null);
    await store.upsertPairing(account.id, host.id, client.id, 'interact');
    await store.upsertPushRegistration(client.id, 'ab'.repeat(32), at);
    await store.revokeDevice(client.id, at);
    assert.equal(await store.getPushRegistration(client.id), null);
    assert.equal(await store.deletePushRegistration(client.id), false);
  });

  it('atomically claims and finalizes exact-key enrollment exactly once', async () => {
    const account = await store.createAccount({
      id: 'acct_' + '2'.repeat(32), label: 'Remote', tokenDigest: 'remote-account-digest',
    });
    const host = await store.createDevice({
      id: 'dev_' + '3'.repeat(32), accountId: account.id, name: 'Mac', platform: 'macos',
      role: 'host', publicKey: '34'.repeat(32), tokenDigest: 'remote-host-digest',
    });
    const enrollment = await store.createRemoteEnrollment({
      id: 'enr_' + '4'.repeat(32), accountId: account.id, hostDeviceId: host.id,
      roomId: 'B'.repeat(43), admissionDigest: 'enrollment-digest', expiresAt: now + 300,
    });
    const claim = (suffix: string) => store.claimRemoteEnrollment({
      id: enrollment.id, admissionDigest: enrollment.admissionDigest,
      provisionalDeviceId: 'dev_' + suffix.repeat(32), provisionalName: 'Phone',
      provisionalPlatform: 'ios', provisionalPublicKey: suffix.repeat(64).slice(0, 64),
      provisionalTokenDigest: `token-${suffix}`, now,
    });
    assert.deepEqual((await Promise.all([claim('5'), claim('6')])).sort(), [false, true]);
    const claimed = await store.getRemoteEnrollment(enrollment.id, now);
    assert.ok(claimed?.provisionalDeviceId);
    const finalize = () => store.finalizeRemoteEnrollment({
      id: enrollment.id,
      hostDeviceId: host.id,
      controllerPublicKey: claimed!.provisionalPublicKey!,
      permission: 'control',
      linkId: 'link_' + '7'.repeat(32),
      linkRoomId: 'C'.repeat(43),
      completedAt: new Date(now * 1000).toISOString(),
      now,
      maxDevices: 32,
    });
    const finalized = await Promise.all([finalize(), finalize()]);
    assert.equal(finalized.filter(Boolean).length, 1);
    const link = finalized.find(Boolean)!;
    assert.equal((await store.getDevice(claimed!.provisionalDeviceId!))?.publicKey, claimed!.provisionalPublicKey);
    assert.equal((await store.getPairing(host.id, claimed!.provisionalDeviceId!))?.permission, 'control');
    assert.equal((await store.getRemoteLink(link.id))?.roomId, 'C'.repeat(43));
    assert.notEqual((await store.getRemoteEnrollment(enrollment.id, now))?.completedAt, null);
  });

  it('retired the ICE signaling tables and revokes pairings that predate Remote Link', async () => {
    const tables = await store.pool.query<{ table_name: string }>(
      `SELECT table_name FROM information_schema.tables WHERE table_schema = 'public'`,
    );
    const names = new Set(tables.rows.map((row) => row.table_name));
    for (const retired of ['turn_credentials', 'rendezvous_offers', 'presence', 'relay_tickets', 'pairing_requests']) {
      assert.equal(names.has(retired), false, `${retired} should be dropped by 0007`);
    }
    for (const kept of ['accounts', 'devices', 'pairings', 'remote_links', 'remote_admissions', 'relay_revocation_outbox', 'push_registrations']) {
      assert.equal(names.has(kept), true, `${kept} must survive the retirement migration`);
    }

    // A pairing recorded by the retired protocol has no remote_links row. The
    // migration is forward-only, so replay its statements against rows
    // inserted after boot to prove the predicate it uses.
    const account = await store.createAccount({
      id: 'acct_' + '9a'.repeat(16), label: 'Legacy', tokenDigest: 'legacy-account-digest',
    });
    const host = await store.createDevice({
      id: 'dev_' + '9b'.repeat(16), accountId: account.id, name: 'Mac', platform: 'macos',
      role: 'host', publicKey: '21'.repeat(32), tokenDigest: 'legacy-host-digest',
    });
    const legacyPhone = await store.createDevice({
      id: 'dev_' + '9c'.repeat(16), accountId: account.id, name: 'Old phone', platform: 'ios',
      role: 'client', publicKey: '31'.repeat(32), tokenDigest: 'legacy-phone-digest',
    });
    const enrolledPhone = await store.createDevice({
      id: 'dev_' + '9d'.repeat(16), accountId: account.id, name: 'New phone', platform: 'ios',
      role: 'client', publicKey: '41'.repeat(32), tokenDigest: 'enrolled-phone-digest',
    });
    await store.upsertPairing(account.id, host.id, legacyPhone.id, 'control');
    await store.upsertPairing(account.id, host.id, enrolledPhone.id, 'interact');
    await store.getOrCreateRemoteLink(account.id, host.id, enrolledPhone.id, 'B'.repeat(43));

    const migration = (await loadMigrations()).find((file) => file.name === '0007_retire_ice_signaling.sql');
    assert.notEqual(migration, undefined);
    await store.pool.query(migration!.sql);

    assert.equal(await store.getPairing(host.id, legacyPhone.id), null, 'legacy pairing is revoked');
    assert.equal(await store.deviceByTokenDigest(legacyPhone.id, 'legacy-phone-digest'), null, 'legacy phone is revoked');
    assert.equal((await store.getPairing(host.id, enrolledPhone.id))?.permission, 'interact', 'enrolled pairing survives');
    assert.equal((await store.deviceByTokenDigest(enrolledPhone.id, 'enrolled-phone-digest'))?.id, enrolledPhone.id);
    assert.equal((await store.deviceByTokenDigest(host.id, 'legacy-host-digest'))?.id, host.id, 'the Mac identity is kept');
  });

  it('atomically consumes invitations and single-use admissions', async () => {
    const invitation = await store.createOwnerInvitation({
      id: 'inv_' + '8'.repeat(32), secretDigest: 'invitation-digest', expiresAt: now + 600,
    });
    const consumed = await Promise.all([
      store.consumeOwnerInvitation(invitation.id, invitation.secretDigest, new Date().toISOString(), now),
      store.consumeOwnerInvitation(invitation.id, invitation.secretDigest, new Date().toISOString(), now),
    ]);
    assert.deepEqual(consumed.sort(), [false, true]);
    const account = await store.createAccount({
      id: 'acct_' + '9'.repeat(32), label: 'Admission', tokenDigest: 'admission-account-digest',
    });
    const host = await store.createDevice({
      id: 'dev_' + '8'.repeat(32), accountId: account.id, name: 'Mac', platform: 'macos',
      role: 'host', publicKey: '89'.repeat(32), tokenDigest: 'admission-host-digest',
    });
    const enrollment = await store.createRemoteEnrollment({
      id: 'enr_' + '7'.repeat(32), accountId: account.id, hostDeviceId: host.id,
      roomId: 'D'.repeat(43), admissionDigest: 'admission-enrollment-digest', expiresAt: now + 300,
    });
    const admission = await store.createRemoteAdmission({
      id: 'adm_' + '9'.repeat(32), linkId: null, enrollmentId: enrollment.id,
      roomId: enrollment.roomId, role: 'host', purpose: 'enrollment', generation: 1,
      expiresAt: now + 60,
    });
    const redeemed = await Promise.all([
      store.redeemRemoteAdmission(admission.id, 'attempt-one-00001', 'lease_' + 'a'.repeat(32), now + 600, now),
      store.redeemRemoteAdmission(admission.id, 'attempt-two-00002', 'lease_' + 'b'.repeat(32), now + 600, now),
    ]);
    assert.equal(redeemed.filter(Boolean).length, 1);
  });
});
