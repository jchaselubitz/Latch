/**
 * Server assembly.
 *
 * `createServer` is exported separately from `main.ts` so tests can start the
 * real HTTP surface on an ephemeral port against the in-memory store.
 */

import { createServer as createHttpServer } from 'node:http';
import type { Server } from 'node:http';

import { createRouter } from './api.ts';
import type { Config } from './config.ts';
import type { TurnProvider } from './cloudflare-turn.ts';
import { createListener } from './http/router.ts';
import type { RequestLog } from './http/router.ts';
import type { Store } from './store/types.ts';

export interface ServerOptions {
  readonly config: Config;
  readonly store: Store;
  readonly now?: () => number;
  readonly readiness?: () => Promise<{ migrations: string[] }>;
  readonly turn?: TurnProvider | null;
  readonly log?: (entry: RequestLog) => void;
}

/** Structured request log. Deliberately free of identifiers and payloads. */
export function defaultLog(entry: RequestLog): void {
  process.stdout.write(
    `${JSON.stringify({
      level: 'info',
      message: 'request',
      method: entry.method,
      path: entry.path,
      status: entry.status,
      durationMs: Math.round(entry.durationMs * 100) / 100,
    })}\n`,
  );
}

export function createServer(options: ServerOptions): Server {
  const router = createRouter({
    config: options.config,
    store: options.store,
    now: options.now ?? (() => Date.now()),
    readiness: options.readiness ?? (async () => ({ migrations: [] })),
    turn: options.turn ?? null,
  });
  const server = createHttpServer(createListener(router, options.log ?? defaultLog));
  // A slow or absent request line must not hold a connection open forever.
  server.headersTimeout = 10_000;
  server.requestTimeout = 20_000;
  server.keepAliveTimeout = 30_000;
  return server;
}
