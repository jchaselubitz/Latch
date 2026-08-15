/**
 * Standalone migration runner: `npm run migrate`.
 *
 * The service also migrates on boot, but a separate entrypoint lets an
 * operator apply a schema change from a Railway one-off command or a release
 * pipeline without starting a listener.
 */

import { loadConfig } from './config.ts';
import { loadMigrations, runMigrations } from './migrate.ts';
import { PostgresStore } from './store/postgres.ts';

async function main(): Promise<void> {
  const config = loadConfig(process.env);
  const store = new PostgresStore({
    connectionString: config.databaseUrl,
    poolSize: 1,
    sslRejectUnauthorized: config.databaseSslRejectUnauthorized,
  });
  try {
    const result = await runMigrations(store.pool, await loadMigrations());
    process.stdout.write(
      `${JSON.stringify({
        level: 'info',
        message: 'migrations complete',
        applied: result.applied,
        alreadyApplied: result.alreadyApplied.length,
      })}\n`,
    );
  } finally {
    await store.close();
  }
}

main().catch((error: unknown) => {
  process.stderr.write(
    `${JSON.stringify({ level: 'fatal', message: (error as Error).message })}\n`,
  );
  process.exit(1);
});
