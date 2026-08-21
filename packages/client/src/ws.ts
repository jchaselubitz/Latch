export const SOCKET_OPEN = 1;
// Close codes `latch serve` uses to say retrying is pointless.
export const FATAL_CLOSE_CODES = new Set([4404, 1008]);

export function latchProtocols({ token }: { token: string }): string[] {
  return [`latch.v2.${token}`];
}

export function sessionWsUrl({
  baseUrl,
  sessionId,
  channel,
  query
}: {
  baseUrl: string;
  sessionId: string;
  channel: 'terminal' | 'conversation';
  query?: Record<string, string>;
}): string {
  const url = new URL(baseUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.pathname = `/v2/sessions/${encodeURIComponent(sessionId)}/${channel}`;
  url.search = '';
  url.hash = '';
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      url.searchParams.set(key, value);
    }
  }
  return url.toString();
}
