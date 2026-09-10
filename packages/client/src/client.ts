import { attachTerminal } from './terminal.ts';
import { defaultRetryPolicy } from './reconnect.ts';
import type {
  GatewayCapabilities,
  InspectReport,
  LatchClient,
  LatchClientOptions,
  ListReport,
  RetryPolicy,
  StopReport
} from './types.ts';

/** The code a gateway answer carries when it names no more specific one. */
export const GATEWAY_ERROR_REQUEST_FAILED = 'request_failed';
/** Refused by this client: the gateway does not advertise `endpoints.stopSession`. */
export const GATEWAY_ERROR_STOP_UNSUPPORTED = 'stop_unsupported';

export class LatchGatewayError extends Error {
  readonly status: number;
  readonly path: string;
  /**
   * The gateway's stable error code (`session_not_found`,
   * `session_still_running`, ...), or `request_failed` when the answer carried
   * none. Branch on this, not on the human-readable message.
   */
  readonly code: string;

  constructor({
    status,
    path,
    reason,
    code = GATEWAY_ERROR_REQUEST_FAILED
  }: {
    status: number;
    path: string;
    reason: string;
    code?: string;
  }) {
    super(`latch serve ${path} failed (${status}): ${reason}`);
    this.name = 'LatchGatewayError';
    this.status = status;
    this.path = path;
    this.code = code;
  }
}

export function createLatchClient(options: LatchClientOptions): LatchClient {
  const baseUrl = options.url.replace(/\/+$/, '');
  const token = options.token;
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
  const retry: RetryPolicy = options.retry ?? defaultRetryPolicy;

  async function requestJson<T>({
    path,
    method = 'GET'
  }: {
    path: string;
    method?: 'GET' | 'POST';
  }): Promise<T> {
    const response = await fetchImpl(`${baseUrl}${path}`, {
      method,
      headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' }
    });
    if (!response.ok) {
      const detail = await response.text().catch(() => '');
      // A gateway failure is `{ error: <stable code>, reason: <text> }`. Anything
      // else -- a bare 404 from a route the gateway never had, a proxy page --
      // has no code, and its body is only ever a reason.
      let parsed: { error?: unknown; reason?: unknown } | undefined;
      try {
        parsed = JSON.parse(detail) as { error?: unknown; reason?: unknown };
      } catch {
        parsed = undefined;
      }
      const code = typeof parsed?.error === 'string' ? parsed.error : undefined;
      const reason = typeof parsed?.reason === 'string' ? parsed.reason : undefined;
      throw new LatchGatewayError({
        status: response.status,
        path,
        code,
        reason: reason ?? code ?? (detail || 'request failed')
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
    stopSession: async ({ sessionId }) => {
      const path = `/v2/sessions/${encodeURIComponent(sessionId)}/stop`;
      // Ask discovery before asking the gateway. A Mac that predates the route
      // answers a bare 404 on this path, which would read exactly like the
      // session being gone; refusing here keeps those two facts apart.
      const capabilities = await requestJson<GatewayCapabilities>({ path: '/v2/capabilities' });
      if (capabilities.endpoints?.stopSession !== true) {
        throw new LatchGatewayError({
          status: 0,
          path,
          code: GATEWAY_ERROR_STOP_UNSUPPORTED,
          reason: 'this gateway does not serve session stop'
        });
      }
      return requestJson<StopReport>({ path, method: 'POST' });
    },
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
