import type { InteractionCapabilities } from '@latch/harness-schema';

import { attachTerminal } from './terminal';
import { defaultRetryPolicy } from './reconnect';
import type {
  InspectReport,
  LatchClient,
  LatchClientOptions,
  ListReport,
  RetryPolicy
} from './types';

export function createLatchClient(options: LatchClientOptions): LatchClient {
  const baseUrl = options.url.replace(/\/+$/, '');
  const token = options.token;
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
  const retry: RetryPolicy = options.retry ?? defaultRetryPolicy;
  const webSocket = options.webSocket;

  async function requestJson<T>({ path }: { path: string }): Promise<T> {
    const response = await fetchImpl(`${baseUrl}${path}`, {
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: 'application/json'
      }
    });
    if (!response.ok) {
      const detail = await response.text().catch(() => '');
      throw new Error(
        `latch serve ${path} failed (${response.status})${detail ? `: ${detail}` : ''}`
      );
    }
    return (await response.json()) as T;
  }

  return {
    listSessions: () => requestJson<ListReport>({ path: '/v1/sessions' }),
    inspectSession: ({ sessionId }) =>
      requestJson<InspectReport>({ path: `/v1/sessions/${encodeURIComponent(sessionId)}` }),
    sessionCapabilities: ({ sessionId }) =>
      requestJson<InteractionCapabilities>({
        path: `/v1/sessions/${encodeURIComponent(sessionId)}/capabilities`
      }),
    attachTerminal: ({ sessionId }) =>
      attachTerminal({
        baseUrl,
        token,
        sessionId,
        retry,
        webSocket
      })
  };
}
