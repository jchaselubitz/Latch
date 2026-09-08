import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import {
  createOwnerAccount,
  enrollPair,
  OPERATOR_SECRET,
  publicKeyFor,
  RELAY_INVALIDATION_SECRET,
  RELAY_SERVICE_TOKEN,
  startHarness,
} from './test-harness.ts';
import { deliverRelayRevocations } from './revocation-worker.ts';

const payload = (claim: string): Record<string, unknown> =>
  JSON.parse(Buffer.from(claim.split('.')[1]!, 'base64url').toString('utf8')) as Record<string, unknown>;

describe('owner bootstrap', () => {
  it('removes anonymous account creation and consumes an operator invitation once', async () => {
    const harness = await startHarness();
    after(() => harness.close());

    assert.equal((await harness.request('POST', '/v1/accounts', { body: {} })).status, 404);
    assert.equal((await harness.request('POST', '/v1/operator/owner-invitations', { body: {} })).status, 401);
    const invitation = await harness.request('POST', '/v1/operator/owner-invitations', {
      token: OPERATOR_SECRET, body: {},
    });
    assert.equal(invitation.status, 201);
    const first = await harness.request('POST', '/v1/accounts/claim', {
      body: { invitation: invitation.body.invitation, label: 'Owner' },
    });
    assert.equal(first.status, 201);
    const replay = await harness.request('POST', '/v1/accounts/claim', {
      body: { invitation: invitation.body.invitation, label: 'Attacker' },
    });
    assert.equal(replay.status, 403);
  });
});

describe('Remote Link enrollment and admission', () => {
  it('enforces device, owner, source-address, and deployment admission budgets', async () => {
    const harness = await startHarness({
      REMOTE_ADMISSION_RATE_PER_DEVICE: '4',
      REMOTE_ADMISSION_RATE_PER_OWNER: '8',
      REMOTE_ADMISSION_RATE_PER_IP: '8',
      REMOTE_ADMISSION_RATE_GLOBAL: '16',
    });
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    for (let index = 0; index < 4; index += 1) {
      assert.equal((await harness.request('POST', '/v1/relay-admissions', {
        token: client.token, body: { peerDeviceId: host.deviceId },
      })).status, 201);
    }
    assert.equal((await harness.request('POST', '/v1/relay-admissions', {
      token: client.token, body: { peerDeviceId: host.deviceId },
    })).status, 429);
  });

  it('keeps provisional credentials enrollment-only and commits only the exact approved key', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const account = await createOwnerAccount(harness);
    const host = await harness.request('POST', '/v1/devices', {
      token: account.body.accountToken,
      body: { name: 'Mac', platform: 'macos', role: 'host', publicKey: publicKeyFor('ab') },
    });
    const opened = await harness.request('POST', '/v1/enrollments', { token: host.body.deviceToken, body: {} });
    assert.equal(opened.status, 201);
    assert.equal('enrollmentSecret' in opened.body, false);

    const claim = await harness.request('POST', `/v1/enrollments/${opened.body.enrollmentId}/claim`, {
      body: {
        admissionCode: opened.body.admissionCode, name: 'Phone', platform: 'ios',
        publicKey: publicKeyFor('cd'),
      },
    });
    assert.equal(claim.status, 201);
    assert.equal((await harness.request('GET', '/v1/pairings', { token: claim.body.provisionalToken })).status, 401);
    const replay = await harness.request('POST', `/v1/enrollments/${opened.body.enrollmentId}/claim`, {
      body: {
        admissionCode: opened.body.admissionCode, name: 'Other', platform: 'ios',
        publicKey: publicKeyFor('ef'),
      },
    });
    assert.equal(replay.status, 409);
    const wrong = await harness.request('POST', `/v1/enrollments/${opened.body.enrollmentId}/complete`, {
      token: host.body.deviceToken,
      body: { controllerPublicKey: publicKeyFor('ef'), permission: 'control', grantRevision: 1 },
    });
    assert.equal(wrong.status, 403);
    const completed = await harness.request('POST', `/v1/enrollments/${opened.body.enrollmentId}/complete`, {
      token: host.body.deviceToken,
      body: { controllerPublicKey: publicKeyFor('cd'), permission: 'control', grantRevision: 1 },
    });
    assert.equal(completed.status, 200);
    assert.equal((await harness.request('GET', '/v1/pairings', { token: claim.body.provisionalToken })).status, 200);
  });

  it('issues identity-free single-use claims, rejects stale generations, and renews a current lease', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const issue = () => harness.request('POST', '/v1/relay-admissions', {
      token: client.token, body: { peerDeviceId: host.deviceId },
    });
    const stale = await issue();
    const current = await issue();
    assert.equal(current.status, 201);
    const decoded = payload(current.body.admission);
    for (const forbidden of ['accountId', 'deviceId', 'publicKey', 'permission', 'gatewayToken', 'sessionId']) {
      assert.equal(forbidden in decoded, false);
    }
    const redeem = (ticketId: string, attemptId: string) =>
      harness.request('POST', '/private/v1/relay/redemptions', {
        token: RELAY_SERVICE_TOKEN, body: { ticketId, attemptId },
      });
    assert.equal((await redeem(String(payload(stale.body.admission).jti), 'attempt_stale_0001')).status, 403);
    const ticketId = String(decoded.jti);
    const first = await redeem(ticketId, 'attempt_current_01');
    assert.equal(first.status, 200);
    const retry = await redeem(ticketId, 'attempt_current_01');
    assert.deepEqual(retry.body, first.body);
    assert.equal((await redeem(ticketId, 'attempt_attacker_01')).status, 403);
    const renewed = await harness.request('POST', `/v1/relay-leases/${first.body.leaseId}/renew`, {
      token: client.token, body: {},
    });
    assert.equal(renewed.status, 200);
    const extension = payload(renewed.body.extension);
    assert.equal(extension.leaseId, first.body.leaseId);
    assert.equal(extension.roomId, decoded.roomId);
    assert.equal(Number(extension.exp) - Number(extension.nbf), 600);
  });

  it('denies redemption after pairing revocation and durably queues room invalidation', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const issued = await harness.request('POST', '/v1/relay-admissions', {
      token: client.token, body: { peerDeviceId: host.deviceId },
    });
    await harness.request('DELETE', `/v1/pairings/${client.deviceId}`, { token: host.token });
    const denied = await harness.request('POST', '/private/v1/relay/redemptions', {
      token: RELAY_SERVICE_TOKEN,
      body: { ticketId: payload(issued.body.admission).jti, attemptId: 'attempt_revoked_01' },
    });
    assert.equal(denied.status, 403);
    const pending = await harness.store.listPendingRelayRevocations(10);
    assert.equal(pending.length, 1);

    let attempts = 0;
    const failingFetch: typeof fetch = async () => {
      attempts += 1;
      throw new Error('relay unavailable');
    };
    assert.equal(await deliverRelayRevocations({
      store: harness.store,
      relayUrl: 'wss://relay.example/v1/connect',
      secret: RELAY_INVALIDATION_SECRET,
      now: () => harness.nowSeconds() * 1_000,
      fetch: failingFetch,
    }), 0);
    assert.equal(attempts, 1);
    assert.equal((await harness.store.listPendingRelayRevocations(10)).length, 1);

    let deliveredUrl = '';
    const successfulFetch: typeof fetch = async (input, init) => {
      deliveredUrl = String(input);
      assert.equal(init?.headers && (init.headers as Record<string, string>).authorization,
        `Bearer ${RELAY_INVALIDATION_SECRET}`);
      return new Response(null, { status: 204 });
    };
    assert.equal(await deliverRelayRevocations({
      store: harness.store,
      relayUrl: 'wss://relay.example/v1/connect',
      secret: RELAY_INVALIDATION_SECRET,
      now: () => harness.nowSeconds() * 1_000,
      fetch: successfulFetch,
    }), 1);
    assert.equal(deliveredUrl, 'https://relay.example/private/v1/invalidate');
    assert.equal((await harness.store.listPendingRelayRevocations(10)).length, 0);
  });

  it('increments the link grant revision so an existing permission context cannot survive a downgrade', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const before = await harness.request('GET', '/v1/remote-links', { token: host.token });
    assert.equal(before.body.links[0].grantRevision, 1);
    const admission = await harness.request('POST', '/v1/relay-admissions', {
      token: client.token, body: { peerDeviceId: host.deviceId },
    });

    assert.equal((await harness.request('POST', '/v1/pairings', {
      token: host.token,
      body: { clientDeviceId: client.deviceId, permission: 'observe' },
    })).status, 201);
    const afterDowngrade = await harness.request('GET', '/v1/remote-links', { token: host.token });
    assert.equal(afterDowngrade.body.links[0].grantRevision, 2);
    assert.equal((await harness.request('POST', '/private/v1/relay/redemptions', {
      token: RELAY_SERVICE_TOKEN,
      body: { ticketId: payload(admission.body.admission).jti, attemptId: 'attempt_stale_grant' },
    })).status, 403);
  });
});
