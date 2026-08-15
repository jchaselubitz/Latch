/** Cloudflare Realtime TURN API boundary. Long-lived key material stays here. */

export interface IceServer {
  readonly urls: readonly string[];
  readonly username?: string;
  readonly credential?: string;
}

export interface TurnProvider {
  issue(ttlSeconds: number): Promise<readonly IceServer[]>;
  revoke(username: string): Promise<void>;
}

export class CloudflareTurnProvider implements TurnProvider {
  constructor(
    private readonly keyId: string,
    private readonly apiToken: string,
    private readonly fetchImpl: typeof fetch = fetch,
  ) {}

  async issue(ttlSeconds: number): Promise<readonly IceServer[]> {
    const response = await this.fetchImpl(
      `https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(this.keyId)}/credentials/generate-ice-servers`,
      {
        method: 'POST',
        headers: { authorization: `Bearer ${this.apiToken}`, 'content-type': 'application/json' },
        body: JSON.stringify({ ttl: ttlSeconds }),
      },
    );
    if (!response.ok) throw new Error(`Cloudflare TURN credential issuance failed (${response.status})`);
    const payload: unknown = await response.json();
    const servers = payload && typeof payload === 'object' ? (payload as { iceServers?: unknown }).iceServers : null;
    if (!Array.isArray(servers) || !servers.every(validIceServer)) {
      throw new Error('Cloudflare TURN returned an invalid ICE server response');
    }
    return servers;
  }

  async revoke(username: string): Promise<void> {
    const response = await this.fetchImpl(
      `https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(this.keyId)}/credentials/${encodeURIComponent(username)}/revoke`,
      { method: 'POST', headers: { authorization: `Bearer ${this.apiToken}` } },
    );
    if (!response.ok) throw new Error(`Cloudflare TURN credential revocation failed (${response.status})`);
  }
}

function validIceServer(value: unknown): value is IceServer {
  if (!value || typeof value !== 'object') return false;
  const server = value as Record<string, unknown>;
  return Array.isArray(server.urls) && server.urls.every((url) => typeof url === 'string') &&
    (server.username === undefined || typeof server.username === 'string') &&
    (server.credential === undefined || typeof server.credential === 'string');
}
