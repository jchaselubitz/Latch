export const SOCKET_OPEN = 1;
// Close codes `latch serve` uses to say retrying is pointless.
//
// Every reasoned terminal close is fatal, including `stolen`. A terminal
// connection is the session's one exclusive surface, so an automatic reconnect
// after a steal would take the surface back from whoever just claimed it, and
// two clients set to reconnect would trade it forever. Reattaching after a
// steal is a decision for the person at the keyboard. Reconnecting is left to
// transport-level drops, which carry no reasoned code.
export const FATAL_CLOSE_CODES = new Set([1000, 1008, 4400, 4404, 4408, 4409, 4410, 4500]);

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
