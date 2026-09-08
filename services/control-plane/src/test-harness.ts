/**
 * Test harness: the real HTTP server, the in-memory store, and a clock the
 * test controls. Tests exercise the shipped request path rather than calling
 * handlers directly, so routing, body limits, and auth are covered too.
 */

import type { AddressInfo } from 'node:net';
import { generateKeyPairSync } from 'node:crypto';

import { loadConfig } from './config.ts';
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

export const OPERATOR_SECRET = 'operator-test-secret-'.padEnd(48, 'x');
export const RELAY_SERVICE_TOKEN = 'relay-service-test-secret-'.padEnd(48, 'x');
export const RELAY_INVALIDATION_SECRET = 'relay-invalidation-test-'.padEnd(48, 'x');
const TEST_ADMISSION_PRIVATE_KEY = generateKeyPairSync('ed25519').privateKey.export({
  format: 'pem', type: 'pkcs8',
}).toString();

import type { ApnsDelivery, ApnsOutcome, ApnsSender } from './apns.ts';

/** Records deliveries and answers with a scripted outcome per token. */
export class FakeApns implements ApnsSender {
  readonly deliveries: ApnsDelivery[] = [];
  readonly outcomes = new Map<string, ApnsOutcome>();

  async send(delivery: ApnsDelivery): Promise<ApnsOutcome> {
    this.deliveries.push(delivery);
    return this.outcomes.get(delivery.token) ?? 'delivered';
  }
}

export async function startHarness(
  overrides: Record<string, string> = {},
  options: { apns?: ApnsSender | null } = {},
): Promise<Harness> {
  const config = loadConfig({
    DATABASE_URL: 'postgres://unused/test',
    OPERATOR_SECRET,
    RELAY_SERVICE_TOKEN,
    RELAY_INVALIDATION_SECRET,
    ADMISSION_PRIVATE_KEY_PEM: TEST_ADMISSION_PRIVATE_KEY,
    ...overrides,
  });
  const store = new MemoryStore();
  const logs: RequestLog[] = [];
  let clock = Date.UTC(2026, 0, 1, 12, 0, 0);
  const server = createServer({
    config,
    store,
    now: () => clock,
    readiness: async () => ({ migrations: ['0001_initial.sql'] }),
    log: (entry) => logs.push(entry),
    apns: options.apns ?? null,
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${port}`;

  return {
    store,
    config,
    baseUrl,
    logs,
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

export async function createOwnerAccount(harness: Harness, label = 'Test') {
  const invitation = await harness.request('POST', '/v1/operator/owner-invitations', {
    token: OPERATOR_SECRET, body: {},
  });
  return harness.request('POST', '/v1/accounts/claim', {
    body: { invitation: invitation.body.invitation, label },
  });
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
  const account = await createOwnerAccount(harness);
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
