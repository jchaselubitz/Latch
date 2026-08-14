import { attachTerminal } from './terminal.ts';
import { subscribeEvents } from './events.ts';
import { legacyGatewayCapabilities } from './compatibility.ts';
import { defaultRetryPolicy } from './reconnect.ts';
import type {
  GatewayCapabilities,
  InspectReport,
  LatchClient,
  LatchClientOptions,
  ListReport,
  RetryPolicy,
  SendReport,
  SendRequest,
  SessionCapabilities
} from './types.ts';

export class LatchSendError extends Error {
  readonly status: number;
  readonly reason: string;
  readonly refused: boolean;

  constructor({
    status,
    reason,
    refused
  }: {
    status: number;
    reason: string;
    refused: boolean;
  }) {
    super(reason);
    this.name = 'LatchSendError';
    this.status = status;
    this.reason = reason;
    this.refused = refused;
  }
}

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
  const webSocket = options.webSocket;

  async function requestJson<T>({
    path,
    method = 'GET',
    body
  }: {
    path: string;
    method?: string;
    body?: unknown;
  }): Promise<T> {
    const response = await fetchImpl(`${baseUrl}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: 'application/json',
        ...(body !== undefined ? { 'Content-Type': 'application/json' } : {})
      },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {})
    });
    if (!response.ok) {
      throw await errorForResponse({ path, response });
    }
    return (await response.json()) as T;
  }

  async function sessionCapabilities({
    sessionId
  }: {
    sessionId: string;
  }): Promise<SessionCapabilities> {
    return requestJson<SessionCapabilities>({
      path: `/v1/sessions/${encodeURIComponent(sessionId)}/capabilities`
    });
  }

  async function send(request: SendRequest): Promise<SendReport> {
    return requestJson<SendReport>({
      path: `/v1/sessions/${encodeURIComponent(request.sessionId)}/send`,
      method: 'POST',
      body: sendBody({ request })
    });
  }

  return {
    listSessions: () => requestJson<ListReport>({ path: '/v1/sessions' }),
    inspectSession: ({ sessionId }) =>
      requestJson<InspectReport>({ path: `/v1/sessions/${encodeURIComponent(sessionId)}` }),
    gatewayCapabilities: async () => {
      try {
        return await requestJson<GatewayCapabilities>({ path: '/v1/capabilities' });
      } catch (error) {
        if (error instanceof LatchGatewayError && error.status === 404) {
          return legacyGatewayCapabilities();
        }
        throw error;
      }
    },
    sessionCapabilities,
    canSend: sessionCapabilities,
    send,
    attachTerminal: ({ sessionId, cols, rows }) =>
      attachTerminal({
        baseUrl,
        token,
        sessionId,
        cols,
        rows,
        retry,
        webSocket
      }),
    subscribeEvents: ({ sessionId, cursor, connectorEpoch }) =>
      subscribeEvents({
        baseUrl,
        token,
        sessionId,
        cursor,
        connectorEpoch,
        retry,
        webSocket
      })
  };
}

function sendBody({ request }: { request: SendRequest }): Record<string, unknown> {
  if ('message' in request) {
    return { message: request.message };
  }
  if ('keys' in request) {
    return { keys: request.keys };
  }
  return { resolve: request.resolve };
}

async function errorForResponse({
  path,
  response
}: {
  path: string;
  response: Response;
}): Promise<Error> {
  const detail = await response.text().catch(() => '');
  let parsed: { error?: string; reason?: string } | undefined;
  try {
    parsed = JSON.parse(detail) as { error?: string; reason?: string };
  } catch {
    parsed = undefined;
  }
  const reason = parsed?.reason ?? parsed?.error ?? detail;
  if (response.status === 409 && parsed?.error === 'refused') {
    return new LatchSendError({
      status: 409,
      reason: parsed.reason ?? 'refused',
      refused: true
    });
  }
  if (response.status === 409) {
    // 409 also carries "session name is ambiguous", which is a lookup
    // failure, not the harness declining the operation. `refused` is what a
    // Composer shows the user as a reason, so it must not claim that.
    return new LatchSendError({
      status: 409,
      reason: reason || 'conflict',
      refused: false
    });
  }
  return new LatchGatewayError({
    status: response.status,
    path,
    reason: reason || 'request failed'
  });
}
