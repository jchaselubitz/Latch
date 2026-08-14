export const SOCKET_OPEN = 1;
// Close codes `latch serve` uses to say retrying is pointless. 4404 is a
// missing session; 1008 is a policy rejection; 4408 is a session with no
// harness connector.
export const FATAL_CLOSE_CODES = new Set([4404, 1008]);
export const NO_CONNECTOR_CLOSE = 4408;
export const STALE_CURSOR_CLOSE = 4422;

export function latchProtocols({ token }: { token: string }): string[] {
  return [`latch.v1.${token}`];
}

export function sessionWsUrl({
  baseUrl,
  sessionId,
  channel,
  query
}: {
  baseUrl: string;
  sessionId: string;
  channel: 'terminal' | 'events';
  query?: Record<string, string>;
}): string {
  const url = new URL(baseUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.pathname = `/v1/sessions/${encodeURIComponent(sessionId)}/${channel}`;
  url.search = '';
  url.hash = '';
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      url.searchParams.set(key, value);
    }
  }
  return url.toString();
}
