/**
 * Service entrypoint.
 *
 * Boot order is deliberate: validate configuration, connect to PostgreSQL,
 * apply migrations (so a deployment is never serving against an older schema),
 * then start listening. Railway health checks the readiness path, so the
 * listener only reports ready once storage answers.
 */

import { loadConfig } from './config.ts';
import { appliedMigrations, loadMigrations, runMigrations } from './migrate.ts';
import { createServer } from './server.ts';
import { PostgresStore } from './store/postgres.ts';

/** Expired presence, offers, and tickets are swept on this interval. */
const PURGE_INTERVAL_MS = 60_000;

function log(message: string, fields: Record<string, unknown> = {}): void {
  process.stdout.write(`${JSON.stringify({ level: 'info', message, ...fields })}\n`);
}

async function main(): Promise<void> {
  const config = loadConfig(process.env);
  const store = new PostgresStore({
    connectionString: config.databaseUrl,
    poolSize: config.databasePoolSize,
    sslRejectUnauthorized: config.databaseSslRejectUnauthorized,
  });

  if (config.migrateOnBoot) {
    const files = await loadMigrations();
    const result = await runMigrations(store.pool, files);
    log('migrations applied', { applied: result.applied, pending: 0 });
  }

  const server = createServer({
    config,
    store,
    readiness: async () => ({ migrations: await appliedMigrations(store.pool) }),
  });

  const purge = setInterval(() => {
    void store
      .purgeExpired(Math.floor(Date.now() / 1000))
      .catch(() => process.stderr.write('{"level":"error","message":"purge failed"}\n'));
  }, PURGE_INTERVAL_MS);
  purge.unref();

  server.listen(config.port, config.host, () => {
    log('listening', {
      port: config.port,
      environment: config.environment,
      release: config.releaseId,
      relayConfigured: Boolean(config.relayUrl),
    });
  });

  const shutdown = (signal: string): void => {
    log('shutting down', { signal });
    clearInterval(purge);
    server.close(() => {
      void store.close().then(() => process.exit(0));
    });
    // Give in-flight requests a bounded window before forcing exit.
    setTimeout(() => process.exit(0), 10_000).unref();
  };

  process.on('SIGTERM', () => shutdown('SIGTERM'));
  process.on('SIGINT', () => shutdown('SIGINT'));
}

main().catch((error: unknown) => {
  process.stderr.write(
    `${JSON.stringify({ level: 'fatal', message: (error as Error).message })}\n`,
  );
  process.exit(1);
});
