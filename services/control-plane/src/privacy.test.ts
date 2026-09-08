/**
 * Executable trust-boundary checks. The control plane may retain identity,
 * grants, admissions, and leases, but never terminal content or plaintext
 * bearer credentials.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';

import { loadMigrations } from './migrate.ts';
import { enrollPair, startHarness } from './test-harness.ts';

const FORBIDDEN_COLUMN_WORDS = [
  'terminal', 'transcript', 'scrollback', 'output', 'stdout', 'prompt',
  'gateway', 'session_name', 'private_key', 'secret_key', 'command', 'cwd',
  'environment',
];

describe('storage boundary', () => {
  it('has no schema column that could hold terminal content or a gateway token', async () => {
    const sql = (await loadMigrations()).map((migration) => migration.sql).join('\n');
    const columns = [...sql.matchAll(/^\s{4}([a-z_]+)\s+[A-Z]/gm)].map((match) => match[1]!);
    assert.equal(columns.length > 0, true);
    for (const column of columns) {
      for (const word of FORBIDDEN_COLUMN_WORDS) {
        assert.equal(column.includes(word), false, `column ${column} looks like it could hold ${word}`);
      }
    }
  });

  it('stores only digests of credentials it issues', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { accountToken, host, client } = await enrollPair(harness);
    const dump = JSON.stringify(harness.store.snapshot());
    for (const secret of [accountToken, host.token, client.token]) {
      assert.equal(dump.includes(secret), false, 'a plaintext credential reached storage');
      assert.equal(dump.includes(secret.split('.').at(-1)!), false);
    }
  });
});

describe('response and log boundary', () => {
  it('never reissues credentials on reads and logs metadata only', async () => {
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
      assert.equal(body.includes(accountToken), false);
      assert.equal(body.includes(host.token), false);
      assert.equal(body.includes(client.token), false);
    }
    for (const entry of harness.logs) {
      assert.deepEqual(Object.keys(entry).sort(), ['durationMs', 'method', 'path', 'status']);
    }
    const logs = JSON.stringify(harness.logs);
    assert.equal(logs.includes(host.token), false);
    assert.equal(logs.includes(host.deviceId), false);
  });

  it('refuses unknown properties that could smuggle content or credentials', async () => {
    const harness = await startHarness();
    after(() => harness.close());
    const { host, client } = await enrollPair(harness);
    const grant = await harness.request('POST', '/v1/pairings', {
      token: host.token,
      body: {
        clientDeviceId: client.deviceId,
        permission: 'interact',
        terminalOutput: 'secret terminal bytes',
      },
    });
    assert.equal(grant.status, 400);
    assert.equal(grant.body.field, 'terminalOutput');

    const enrollment = await harness.request('POST', '/v1/enrollments', {
      token: host.token,
      body: { gatewayToken: 'lgt_secret' },
    });
    assert.equal(enrollment.status, 400);
    assert.equal(enrollment.body.field, 'gatewayToken');
  });
});
