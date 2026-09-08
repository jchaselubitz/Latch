import { createPrivateKey, sign } from 'node:crypto';

export interface AdmissionPayload {
  readonly iss: string;
  readonly aud: 'latch-relay';
  readonly kid: string;
  readonly roomId: string;
  readonly role: 'host' | 'controller';
  readonly purpose: 'enrollment' | 'session';
  readonly jti: string;
  readonly generation: number;
  readonly nbf: number;
  readonly exp: number;
  readonly limits: {
    readonly maxStreams: 32;
    readonly streamWindowBytes: 262144;
    readonly totalBufferBytes: 8388608;
    readonly recordBytes: 65535;
  };
  readonly leaseId?: string;
}

export const REMOTE_LINK_LIMITS = {
  maxStreams: 32, streamWindowBytes: 262144, totalBufferBytes: 8388608, recordBytes: 65535,
} as const;

/** Produces the compact Ed25519 admission/lease claim consumed by the relay. */
export function signAdmission(payload: AdmissionPayload, privateKeyPem: string): string {
  const header = Buffer.from(JSON.stringify({ alg: 'EdDSA', kid: payload.kid })).toString('base64url');
  const body = Buffer.from(JSON.stringify(payload)).toString('base64url');
  const signature = sign(null, Buffer.from(`${header}.${body}`), createPrivateKey(privateKeyPem));
  return `${header}.${body}.${signature.toString('base64url')}`;
}
