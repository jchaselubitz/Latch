/**
 * Test harness: the real HTTP server, the in-memory store, and a clock the
 * test controls. Tests exercise the shipped request path rather than calling
 * handlers directly, so routing, body limits, and auth are covered too.
 */

import type { AddressInfo } from 'node:net';

import { loadConfig } from './config.ts';
import type { TurnProvider } from './cloudflare-turn.ts';
import type { Config } from './config.ts';
import type { RequestLog } from './http/router.ts';
import { createServer } from './server.ts';
import { MemoryStore } from './store/memory.ts';

export interface Harness {
  readonly store: MemoryStore;
  readonly config: Config;
  readonly baseUrl: string;
  /** Every request log the router emitted, for the privacy assertions. */
  readonly logs: RequestLog[];
  readonly turn: FakeTurnProvider;
  /** Advances the harness clock by `seconds`. */
  advance(seconds: number): void;
  /** Current harness clock in unix seconds. */
  nowSeconds(): number;
  request(
    method: string,
    path: string,
    options?: { token?: string; body?: unknown; headers?: Record<string, string> },
  ): Promise<{ status: number; body: any }>;
  close(): Promise<void>;
}

export const CLOUDFLARE_TURN_KEY_ID = 'a'.repeat(32);
export const CLOUDFLARE_TURN_API_TOKEN = 't'.repeat(48);

export class FakeTurnProvider implements TurnProvider {
  readonly revoked: string[] = [];
  issued = 0;
  async issue(_ttlSeconds: number) {
    this.issued += 1;
    return [
      { urls: ['stun:stun.cloudflare.com:3478'] },
      { urls: ['turn:turn.cloudflare.com:3478?transport=udp'], username: `turn-user-${this.issued}`, credential: `turn-password-${this.issued}-only-returned-to-device` },
    ];
  }
  stunServers() { return [{ urls: ['stun:stun.cloudflare.com:3478'] }]; }
  async revoke(username: string) { this.revoked.push(username); }
}

export async function startHarness(overrides: Record<string, string> = {}): Promise<Harness> {
  const config = loadConfig({
    DATABASE_URL: 'postgres://unused/test',
    CLOUDFLARE_TURN_KEY_ID,
    CLOUDFLARE_TURN_API_TOKEN,
    ...overrides,
  });
  const store = new MemoryStore();
  const turn = new FakeTurnProvider();
  const logs: RequestLog[] = [];
  let clock = Date.UTC(2026, 0, 1, 12, 0, 0);
  const server = createServer({
    config,
    store,
    now: () => clock,
    readiness: async () => ({ migrations: ['0001_initial.sql'] }),
    turn,
    log: (entry) => logs.push(entry),
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${port}`;

  return {
    store,
    config,
    baseUrl,
    logs,
    turn,
    advance(seconds) {
      clock += seconds * 1000;
    },
    nowSeconds() {
      return Math.floor(clock / 1000);
    },
    async request(method, path, options = {}) {
      const headers: Record<string, string> = { ...options.headers };
      if (options.token) {
        headers.authorization = `Bearer ${options.token}`;
      }
      if (options.body !== undefined) {
        headers['content-type'] = 'application/json';
      }
      const response = await fetch(`${baseUrl}${path}`, {
        method,
        headers,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
      });
      const text = await response.text();
      return { status: response.status, body: text ? JSON.parse(text) : null };
    },
    async close() {
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    },
  };
}

export const publicKeyFor = (seed: string): string =>
  seed.repeat(64).slice(0, 64).replace(/[^0-9a-f]/g, '0');

export interface EnrolledDevice {
  readonly deviceId: string;
  readonly token: string;
}

/** Registers an account plus a host and a client device, and pairs them. */
export async function enrollPair(
  harness: Harness,
  permission: 'observe' | 'interact' | 'control' = 'interact',
): Promise<{
  accountId: string;
  accountToken: string;
  host: EnrolledDevice;
  client: EnrolledDevice;
}> {
  const account = await harness.request('POST', '/v1/accounts', { body: { label: 'Test' } });
  const accountToken = account.body.accountToken as string;
  const host = await harness.request('POST', '/v1/devices', {
    token: accountToken,
    body: { name: 'Mac', platform: 'macos', role: 'host', publicKey: publicKeyFor('ab') },
  });
  const client = await harness.request('POST', '/v1/devices', {
    token: accountToken,
    body: { name: 'Phone', platform: 'ios', role: 'client', publicKey: publicKeyFor('cd') },
  });
  await harness.request('POST', '/v1/pairings', {
    token: host.body.deviceToken,
    body: { clientDeviceId: client.body.deviceId, permission },
  });
  return {
    accountId: account.body.accountId,
    accountToken,
    host: { deviceId: host.body.deviceId, token: host.body.deviceToken },
    client: { deviceId: client.body.deviceId, token: client.body.deviceToken },
  };
}

/** A candidate that is valid relative to the harness clock. */
export function candidate(harness: Harness, address = '203.0.113.7:41234', lifetime = 60) {
  return { address, expiresAt: harness.nowSeconds() + lifetime };
}

/** A fully described candidate for ICE signaling contract tests. */
export function iceCandidate(harness: Harness, address = '203.0.113.7:41234', lifetime = 60) {
  return {
    ...candidate(harness, address, lifetime),
    type: 'host',
    priority: 2_130_706_431,
    foundation: '1',
    component: 1,
    protocol: 'udp',
  };
}
