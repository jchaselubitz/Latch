/** Cloudflare Realtime TURN API boundary. Long-lived key material stays here. */

export interface IceServer {
  readonly urls: readonly string[];
  readonly username?: string;
  readonly credential?: string;
}

export interface TurnProvider {
  issue(ttlSeconds: number): Promise<readonly IceServer[]>;
  /** Public STUN configuration, intentionally independent of relay policy. */
  stunServers(): readonly IceServer[];
  revoke(username: string): Promise<void>;
}

export class CloudflareTurnProvider implements TurnProvider {
  private readonly keyId: string;
  private readonly apiToken: string;
  private readonly fetchImpl: typeof fetch;

  constructor(
    keyId: string,
    apiToken: string,
    fetchImpl: typeof fetch = fetch,
  ) {
    this.keyId = keyId;
    this.apiToken = apiToken;
    this.fetchImpl = fetchImpl;
  }

  async issue(ttlSeconds: number): Promise<readonly IceServer[]> {
    const response = await this.fetchImpl(
      `https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(this.keyId)}/credentials/generate-ice-servers`,
      {
        method: 'POST',
        headers: { authorization: `Bearer ${this.apiToken}`, 'content-type': 'application/json' },
        body: JSON.stringify({ ttl: ttlSeconds }),
      },
    );
    // An unrecognised key id answers 404, not 401, so a misconfigured relay
    // reaches the caller as a failure to issue rather than as a device
    // authorization problem.
    if (!response.ok) throw new Error(`Cloudflare TURN credential issuance failed (${response.status})`);
    const payload: unknown = await response.json();
    const iceServers = payload && typeof payload === 'object'
      ? (payload as { iceServers?: unknown }).iceServers
      : null;
    // `iceServers` is an array in the endpoint Cloudflare documents today —
    // one entry for STUN and one for TURN — but the same field has been
    // documented unwrapped, and a single object is trivially the one-entry
    // case. Accepting both costs a line and removes a shape change as a way
    // for relay to stop working.
    const servers = Array.isArray(iceServers) ? iceServers : [iceServers];
    if (!servers.every(validIceServer)) {
      throw new Error('Cloudflare TURN returned an invalid ICE server response');
    }
    return servers;
  }

  stunServers(): readonly IceServer[] {
    return [{ urls: ['stun:stun.cloudflare.com:3478'] }];
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
