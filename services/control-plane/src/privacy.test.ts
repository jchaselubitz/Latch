/**
 * Executable form of the trust boundary in docs/REMOTE_ACCESS_THREAT_MODEL.md.
 *
 * These tests fail if a future change lets terminal content, a gateway
 * credential, or a device secret reach control-plane storage, responses, or
 * logs.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import { loadMigrations } from './migrate.ts';
import { candidate, enrollPair, iceCandidate, startHarness } from './test-harness.ts';

const FORBIDDEN_COLUMN_WORDS = [
  'terminal',
  'transcript',
  'scrollback',
  'output',
  'stdout',
  'prompt',
  'gateway',
  'session_name',
  'private_key',
  'secret_key',
  'command',
  'cwd',
  'environment',
];

describe('storage boundary', () => {
  it('has no schema column that could hold terminal content or a gateway token', async () => {
    const sql = (await loadMigrations()).map((migration) => migration.sql).join('\n');
    const columns = [...sql.matchAll(/^\s{4}([a-z_]+)\s+[A-Z]/gm)].map((match) => match[1]!);
    assert.equal(columns.length > 0, true);
    for (const column of columns) {
      for (const word of FORBIDDEN_COLUMN_WORDS) {
        assert.equal(
          column.includes(word),
          false,
          `column ${column} looks like it could hold ${word}`,
        );
      }
    }
  });

  it('stores only digests of the credentials it issues', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const credentials = await harness.request('POST', '/v1/turn-credentials', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });
    await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness)] },
    });

    const dump = JSON.stringify(harness.store.snapshot());
    for (const secret of [
      accountToken,
      host.token,
      client.token,
      credentials.body.iceServers[1].credential,
    ]) {
      assert.equal(dump.includes(secret), false, 'a plaintext credential reached storage');
      // The secret half alone must not appear either.
      assert.equal(dump.includes(secret.split('.').at(-1)!), false);
    }
  });
});

describe('response and log boundary', () => {
  it('never re-issues a credential on a later read', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);

    const reads = await Promise.all([
      harness.request('GET', '/v1/account', { token: accountToken }),
      harness.request('GET', '/v1/devices', { token: host.token }),
      harness.request('GET', '/v1/pairings', { token: client.token }),
      harness.request('GET', '/v1/account/events', { token: accountToken }),
    ]);
    for (const read of reads) {
      const body = JSON.stringify(read.body);
      assert.equal(body.includes('Token'), false);
      assert.equal(body.includes(accountToken), false);
      assert.equal(body.includes(host.token), false);
    }
  });

  it('logs method, path, status, and duration only', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await enrollPair(harness);
    await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness, '198.51.100.22:33445')] },
    });

    assert.equal(harness.logs.length > 0, true);
    for (const entry of harness.logs) {
      assert.deepEqual(Object.keys(entry).sort(), ['durationMs', 'method', 'path', 'status']);
    }
    const serialized = JSON.stringify(harness.logs);
    assert.equal(serialized.includes('198.51.100.22'), false);
    assert.equal(serialized.includes(host.token), false);
    assert.equal(serialized.includes(host.deviceId), false);
  });

  it('refuses unknown request properties so content cannot be smuggled in', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);

    const withExtras = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: {
        candidates: [candidate(harness)],
        terminalOutput: 'total 0\ndrwxr-xr-x  jake  staff',
      },
    });
    assert.equal(withExtras.status, 400);
    assert.equal(withExtras.body.field, 'terminalOutput');

    const ticketWithExtras = await harness.request('POST', '/v1/turn-credentials', {
      token: client.token,
      body: { peerDeviceId: host.deviceId, gatewayToken: 'lgt_secret' },
    });
    assert.equal(ticketWithExtras.status, 400);

    const candidateWithExtras = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: {
        candidates: [{ ...candidate(harness), sessionName: 'deploy prod' }],
      },
    });
    assert.equal(candidateWithExtras.status, 400);
  });

  it('accepts only structured ICE transport metadata, never hostnames, loopback, or content fields', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host } = await enrollPair(harness);

    const loopback = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness, '127.0.0.1:41234')] },
    });
    assert.equal(loopback.status, 400);

    const mappedLoopback = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: { candidates: [candidate(harness, '[::ffff:7f00:1]:41234')] },
    });
    assert.equal(mappedLoopback.status, 400);

    const hostnameRelatedAddress = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: {
        candidates: [{
          ...iceCandidate(harness),
          type: 'srflx',
          relatedAddress: 'gateway.internal',
          relatedPort: 41234,
        }],
      },
    });
    assert.equal(hostnameRelatedAddress.status, 400);

    const loopbackRelatedAddress = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: {
        candidates: [{
          ...iceCandidate(harness),
          type: 'srflx',
          relatedAddress: '::1',
          relatedPort: 41234,
        }],
      },
    });
    assert.equal(loopbackRelatedAddress.status, 400);

    const contentBearingIceField = await harness.request('POST', '/v1/presence', {
      token: host.token,
      body: {
        candidates: [iceCandidate(harness)],
        iceUfrag: 'validUfrag_123',
        icePwd: 'terminal output: whoami and cwd',
      },
    });
    assert.equal(contentBearingIceField.status, 400);
  });

  it('returns only Cloudflare ICE fields, not device or account detail', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const ticket = await harness.request('POST', '/v1/turn-credentials', {
      token: client.token,
      body: { peerDeviceId: host.deviceId },
    });
    assert.equal(ticket.status, 201);
    assert.deepEqual(Object.keys(ticket.body).sort(), ['expiresAt', 'iceServers']);
  });
});
