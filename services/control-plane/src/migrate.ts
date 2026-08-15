/**
 * Forward-only SQL migrations.
 *
 * Files in `migrations/` are applied in filename order inside one transaction
 * each, guarded by a PostgreSQL advisory lock so two Railway replicas booting
 * together cannot race. Applied files are checksummed: editing a migration
 * that already ran is an error rather than a silent divergence between the
 * deployed database and the repository.
 */

import { createHash } from 'node:crypto';
import { readdir, readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Advisory lock key. Arbitrary but stable and specific to this service. */
const LOCK_KEY = 4_121_990_001;

export interface MigrationFile {
  readonly name: string;
  readonly sql: string;
  readonly checksum: string;
}

export interface MigrationClient {
  query(text: string, values?: unknown[]): Promise<{ rows: Record<string, unknown>[] }>;
}

/** Resolves the migrations directory for both `src/` and compiled `dist/`. */
export function migrationsDirectory(): string {
  return join(dirname(fileURLToPath(import.meta.url)), '..', 'migrations');
}

export function checksumOf(sql: string): string {
  return createHash('sha256').update(sql).digest('hex');
}

export async function loadMigrations(directory = migrationsDirectory()): Promise<MigrationFile[]> {
  const entries = (await readdir(directory)).filter((name) => name.endsWith('.sql')).sort();
  const files: MigrationFile[] = [];
  for (const name of entries) {
    const sql = await readFile(join(directory, name), 'utf8');
    files.push({ name, sql, checksum: checksumOf(sql) });
  }
  return files;
}

export interface MigrationResult {
  readonly applied: string[];
  readonly alreadyApplied: string[];
}

/**
 * Applies every pending migration. Safe to call on each boot: it is a no-op
 * once the database is current.
 */
export async function runMigrations(
  client: MigrationClient,
  files: MigrationFile[],
): Promise<MigrationResult> {
  await client.query('SELECT pg_advisory_lock($1)', [LOCK_KEY]);
  try {
    await client.query(`
      CREATE TABLE IF NOT EXISTS schema_migrations (
        name        TEXT PRIMARY KEY,
        checksum    TEXT        NOT NULL,
        applied_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
      )
    `);
    const { rows } = await client.query('SELECT name, checksum FROM schema_migrations');
    const applied = new Map(rows.map((row) => [String(row.name), String(row.checksum)]));

    const result: MigrationResult = { applied: [], alreadyApplied: [] };
    for (const file of files) {
      const previous = applied.get(file.name);
      if (previous) {
        if (previous !== file.checksum) {
          throw new Error(
            `migration ${file.name} changed after it was applied; add a new migration instead`,
          );
        }
        result.alreadyApplied.push(file.name);
        continue;
      }
      await client.query('BEGIN');
      try {
        await client.query(file.sql);
        await client.query('INSERT INTO schema_migrations (name, checksum) VALUES ($1, $2)', [
          file.name,
          file.checksum,
        ]);
        await client.query('COMMIT');
      } catch (error) {
        await client.query('ROLLBACK');
        throw new Error(`migration ${file.name} failed: ${(error as Error).message}`, {
          cause: error,
        });
      }
      result.applied.push(file.name);
    }
    return result;
  } finally {
    await client.query('SELECT pg_advisory_unlock($1)', [LOCK_KEY]);
  }
}

/** Names of migrations recorded as applied, for the readiness endpoint. */
export async function appliedMigrations(client: MigrationClient): Promise<string[]> {
  const { rows } = await client.query(
    'SELECT name FROM schema_migrations ORDER BY name ASC',
  );
  return rows.map((row) => String(row.name));
}
