import { attachTerminal } from './terminal.ts';
import { defaultRetryPolicy } from './reconnect.ts';
import type {
  GatewayCapabilities,
  InspectReport,
  LatchClient,
  LatchClientOptions,
  ListReport,
  RetryPolicy
} from './types.ts';

export class LatchGatewayError extends Error {
  readonly status: number;
  readonly path: string;

  constructor({ status, path, reason }: { status: number; path: string; reason: string }) {
    super(`latch serve ${path} failed (${status}): ${reason}`);
    this.name = 'LatchGatewayError';
    this.status = status;
    this.path = path;
  }
}

export function createLatchClient(options: LatchClientOptions): LatchClient {
  const baseUrl = options.url.replace(/\/+$/, '');
  const token = options.token;
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
  const retry: RetryPolicy = options.retry ?? defaultRetryPolicy;

  async function requestJson<T>({ path }: { path: string }): Promise<T> {
    const response = await fetchImpl(`${baseUrl}${path}`, {
      headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' }
    });
    if (!response.ok) {
      const detail = await response.text().catch(() => '');
      let parsed: { error?: string } | undefined;
      try {
        parsed = JSON.parse(detail) as { error?: string };
      } catch {
        parsed = undefined;
      }
      throw new LatchGatewayError({
        status: response.status,
        path,
        reason: parsed?.error ?? detail ?? 'request failed'
      });
    }
    return (await response.json()) as T;
  }

  return {
    listSessions: () => requestJson<ListReport>({ path: '/v2/sessions' }),
    inspectSession: ({ sessionId }) =>
      requestJson<InspectReport>({ path: `/v2/sessions/${encodeURIComponent(sessionId)}` }),
    gatewayCapabilities: () =>
      requestJson<GatewayCapabilities>({ path: '/v2/capabilities' }),
    attachTerminal: ({ sessionId, cols, rows }) => {
      const handle = attachTerminal({
        baseUrl,
        token,
        sessionId,
        cols,
        rows,
        retry,
        webSocket: options.webSocket
      });
      // A terminal connection takes the session's only surface. A gateway
      // that predates the exclusive cutover still speaks protocol 2 but does
      // not, so attaching to one would silently be some other behaviour --
      // exactly the mixed-version operation this release does not support.
      // The check runs beside the connection rather than before it so the
      // handle stays synchronous; a gateway that fails it is closed before
      // anyone can type into it.
      void requestJson<GatewayCapabilities>({ path: '/v2/capabilities' })
        .then((capabilities) => {
          if (!capabilities.features?.exclusiveTerminal) {
            handle.close();
          }
        })
        .catch(() => handle.close());
      return handle;
    }
  };
}
