/**
 * Generic APNs attention delivery.
 *
 * The payload is fixed and content-free: every notification says the same
 * sentence. Which session, what it printed, what it is asking, and where it
 * lives are fetched by the phone over the authenticated Remote Link after it
 * opens. The only per-notification inputs are the opaque device token and an
 * opaque collapse id derived from the Mac's event id.
 *
 * Token-based authentication (ES256 JWT) is used so no certificate has to be
 * renewed. Node's `fetch` cannot speak HTTP/2, which APNs requires, so the
 * sender uses `node:http2` directly and opens one short-lived session per
 * delivery; volume here is a handful of alerts per day.
 */

import { createSign, createPrivateKey } from 'node:crypto';
import { connect } from 'node:http2';

export type ApnsEnvironment = 'sandbox' | 'production';

export interface ApnsConfig {
  readonly keyId: string;
  readonly teamId: string;
  readonly privateKeyPem: string;
  /** The app bundle identifier. */
  readonly topic: string;
  readonly environment: ApnsEnvironment;
}

export type ApnsOutcome = 'delivered' | 'invalid_token' | 'failed';

export interface ApnsDelivery {
  /** Hex APNs device token. */
  readonly token: string;
  /** Opaque collapse identifier; identical repeats replace each other. */
  readonly collapseId: string;
  /** Unix seconds after which APNs discards an undelivered alert. */
  readonly expiresAt: number;
}

export interface ApnsSender {
  send(delivery: ApnsDelivery): Promise<ApnsOutcome>;
}

/** The only payload this service ever sends. */
export const ATTENTION_PAYLOAD = Object.freeze({
  aps: {
    alert: { title: 'Latch', body: 'Your Mac needs your attention.' },
    sound: 'default',
    'thread-id': 'latch-attention',
  },
});

const INVALID_TOKEN_REASONS = new Set(['BadDeviceToken', 'Unregistered', 'DeviceTokenNotForTopic']);
const JWT_LIFETIME_SECONDS = 50 * 60;

export function apnsHost(environment: ApnsEnvironment): string {
  return environment === 'production'
    ? 'https://api.push.apple.com'
    : 'https://api.sandbox.push.apple.com';
}

/** Classifies an APNs response. Exported so the mapping is unit-tested. */
export function classifyResponse(status: number, body: string): ApnsOutcome {
  if (status === 200) return 'delivered';
  let reason = '';
  try {
    reason = String((JSON.parse(body) as { reason?: unknown }).reason ?? '');
  } catch {
    reason = '';
  }
  if ((status === 400 || status === 410) && INVALID_TOKEN_REASONS.has(reason)) return 'invalid_token';
  return 'failed';
}

export class Http2ApnsSender implements ApnsSender {
  readonly #config: ApnsConfig;
  readonly #now: () => number;
  #jwt: { value: string; issuedAt: number } | null = null;

  constructor(config: ApnsConfig, now: () => number = () => Date.now()) {
    this.#config = config;
    this.#now = now;
  }

  /** Provider token, cached inside Apple's one-hour validity window. */
  bearer(): string {
    const nowSeconds = Math.floor(this.#now() / 1000);
    if (this.#jwt && nowSeconds - this.#jwt.issuedAt < JWT_LIFETIME_SECONDS) return this.#jwt.value;
    const encode = (value: unknown): string => Buffer.from(JSON.stringify(value)).toString('base64url');
    const header = encode({ alg: 'ES256', kid: this.#config.keyId });
    const claims = encode({ iss: this.#config.teamId, iat: nowSeconds });
    const signer = createSign('SHA256');
    signer.update(`${header}.${claims}`);
    const signature = signer
      .sign({ key: createPrivateKey(this.#config.privateKeyPem), dsaEncoding: 'ieee-p1363' })
      .toString('base64url');
    this.#jwt = { value: `${header}.${claims}.${signature}`, issuedAt: nowSeconds };
    return this.#jwt.value;
  }

  async send(delivery: ApnsDelivery): Promise<ApnsOutcome> {
    if (!/^[0-9a-f]{64}$/.test(delivery.token)) return 'invalid_token';
    const session = connect(apnsHost(this.#config.environment));
    try {
      return await new Promise<ApnsOutcome>((resolve) => {
        const finish = (outcome: ApnsOutcome) => resolve(outcome);
        session.once('error', () => finish('failed'));
        const request = session.request({
          ':method': 'POST',
          ':path': `/3/device/${delivery.token}`,
          authorization: `bearer ${this.bearer()}`,
          'apns-topic': this.#config.topic,
          'apns-push-type': 'alert',
          'apns-priority': '10',
          'apns-expiration': String(delivery.expiresAt),
          'apns-collapse-id': delivery.collapseId.slice(0, 64),
          'content-type': 'application/json',
        });
        let status = 0;
        const chunks: Buffer[] = [];
        request.setTimeout(10_000, () => { request.close(); finish('failed'); });
        request.on('response', (headers) => { status = Number(headers[':status'] ?? 0); });
        request.on('data', (chunk: Buffer) => { chunks.push(chunk); });
        request.on('end', () => finish(classifyResponse(status, Buffer.concat(chunks).toString('utf8'))));
        request.on('error', () => finish('failed'));
        request.end(JSON.stringify(ATTENTION_PAYLOAD));
      });
    } finally {
      session.close();
    }
  }
}
