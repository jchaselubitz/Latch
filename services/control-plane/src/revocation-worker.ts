import type { Store } from './store/types.ts';

export interface RevocationWorkerOptions {
  readonly store: Store;
  readonly relayUrl: string;
  readonly secret: string;
  readonly now?: () => number;
  readonly fetch?: typeof globalThis.fetch;
}

/** Delivers one bounded batch; rows remain durable until the relay acknowledges. */
export async function deliverRelayRevocations(options: RevocationWorkerOptions): Promise<number> {
  const request = options.fetch ?? globalThis.fetch;
  const now = options.now ?? (() => Date.now());
  const endpoint = new URL(options.relayUrl);
  endpoint.protocol = endpoint.protocol === 'wss:' ? 'https:' : 'http:';
  endpoint.pathname = '/private/v1/invalidate';
  endpoint.search = '';
  const pending = await options.store.listPendingRelayRevocations(32);
  let delivered = 0;
  for (const entry of pending) {
    // Past the hard lease horizon there can be no live socket left to close.
    if (entry.notAfter <= Math.floor(now() / 1000)) {
      await options.store.acknowledgeRelayRevocation(entry.id, new Date(now()).toISOString());
      delivered += 1;
      continue;
    }
    try {
      const response = await request(endpoint, {
        method: 'POST',
        headers: { authorization: `Bearer ${options.secret}`, 'content-type': 'application/json' },
        body: JSON.stringify({ roomId: entry.roomId, notAfter: entry.notAfter }),
      });
      if (!response.ok) continue;
      await options.store.acknowledgeRelayRevocation(entry.id, new Date(now()).toISOString());
      delivered += 1;
    } catch {
      // The next worker tick retries the still-unacknowledged row.
    }
  }
  return delivered;
}
