/**
 * Opaque identifiers and bearer credentials.
 *
 * Credentials are high-entropy random strings, so a single SHA-256 digest is
 * an adequate verifier: there is no low-entropy password to brute force. Only
 * digests are persisted, and comparison is constant time.
 */

import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';

const ID_BYTES = 16;
const SECRET_BYTES = 32;

/** Domain separation keeps a digest from being replayed across credential kinds. */
export type CredentialKind = 'account' | 'device' | 'relay-ticket' | 'pairing';

export interface IssuedCredential {
  /** Returned to the caller exactly once. */
  readonly token: string;
  /** Persisted verifier. */
  readonly digest: string;
}

/** Generates an opaque, URL-safe identifier. */
export function newId(prefix: string): string {
  return `${prefix}_${randomBytes(ID_BYTES).toString('hex')}`;
}

/** Generates a hex secret of the relay's expected width. */
export function newSecret(): string {
  return randomBytes(SECRET_BYTES).toString('hex');
}

export function digestOf(kind: CredentialKind, token: string): string {
  return createHash('sha256').update(`latch/v1/${kind} ${token}`).digest('hex');
}

/** Issues a bearer token bound to `subject` so a stray token names its owner. */
export function issueCredential(kind: CredentialKind, subject: string): IssuedCredential {
  const token = `${subject}.${newSecret()}`;
  return { token, digest: digestOf(kind, token) };
}

/** Splits `subject.secret` without revealing whether the secret was well formed. */
export function subjectOf(token: string): string | null {
  const separator = token.indexOf('.');
  if (separator <= 0 || separator === token.length - 1) {
    return null;
  }
  return token.slice(0, separator);
}

export function digestsMatch(left: string, right: string): boolean {
  const a = Buffer.from(left, 'utf8');
  const b = Buffer.from(right, 'utf8');
  if (a.length !== b.length) {
    return false;
  }
  return timingSafeEqual(a, b);
}

/** Extracts a bearer token from an `Authorization` header value. */
export function bearerToken(header: string | undefined): string | null {
  if (!header) {
    return null;
  }
  const match = /^Bearer\s+([A-Za-z0-9._-]+)$/.exec(header.trim());
  return match?.[1] ?? null;
}
