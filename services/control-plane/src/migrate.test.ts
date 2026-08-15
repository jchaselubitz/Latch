import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { appliedMigrations, loadMigrations, runMigrations } from './migrate.ts';
import type { MigrationClient } from './migrate.ts';

/** Records statements and answers the two queries the runner reads back. */
class FakeClient implements MigrationClient {
  readonly statements: string[] = [];
  readonly applied = new Map<string, string>();
  failOn: string | null = null;

  async query(text: string, values: unknown[] = []): Promise<{ rows: Record<string, unknown>[] }> {
    this.statements.push(text.trim().split('\n')[0]!.trim());
    if (text.startsWith('SELECT name, checksum')) {
      return {
        rows: [...this.applied].map(([name, checksum]) => ({ name, checksum })),
      };
    }
    if (text.startsWith('SELECT name FROM schema_migrations')) {
      return { rows: [...this.applied.keys()].sort().map((name) => ({ name })) };
    }
    if (text.startsWith('INSERT INTO schema_migrations')) {
      this.applied.set(String(values[0]), String(values[1]));
      return { rows: [] };
    }
    if (this.failOn && text.includes(this.failOn)) {
      throw new Error('syntax error');
    }
    return { rows: [] };
  }
}

describe('migrations', () => {
  it('loads the repository migrations in filename order', async () => {
    const files = await loadMigrations();
    assert.equal(files.length > 0, true);
    assert.equal(files[0]!.name, '0001_initial.sql');
    assert.deepEqual([...files].sort((a, b) => a.name.localeCompare(b.name)), files);
    assert.match(files[0]!.checksum, /^[0-9a-f]{64}$/);
  });

  it('applies pending migrations once and is idempotent on the next boot', async () => {
    const client = new FakeClient();
    const files = await loadMigrations();

    const first = await runMigrations(client, files);
    assert.deepEqual(first.applied, files.map((file) => file.name));
    assert.deepEqual(first.alreadyApplied, []);

    const second = await runMigrations(client, files);
    assert.deepEqual(second.applied, []);
    assert.deepEqual(second.alreadyApplied, files.map((file) => file.name));
    assert.deepEqual(await appliedMigrations(client), files.map((file) => file.name));
  });

  it('takes and releases the advisory lock so replicas cannot race', async () => {
    const client = new FakeClient();
    await runMigrations(client, await loadMigrations());
    assert.equal(client.statements[0], 'SELECT pg_advisory_lock($1)');
    assert.equal(client.statements.at(-1), 'SELECT pg_advisory_unlock($1)');
  });

  it('refuses a migration whose contents changed after it was applied', async () => {
    const client = new FakeClient();
    const files = await loadMigrations();
    await runMigrations(client, files);
    const tampered = [{ ...files[0]!, sql: '-- edited', checksum: 'deadbeef' }];
    await assert.rejects(() => runMigrations(client, tampered), /changed after it was applied/);
  });

  it('rolls back and reports the failing migration', async () => {
    const client = new FakeClient();
    client.failOn = 'CREATE TABLE accounts';
    const files = await loadMigrations();
    await assert.rejects(
      () => runMigrations(client, files),
      /migration 0001_initial\.sql failed/,
    );
    assert.equal(client.statements.includes('ROLLBACK'), true);
    assert.equal(client.statements.at(-1), 'SELECT pg_advisory_unlock($1)');
  });
});
