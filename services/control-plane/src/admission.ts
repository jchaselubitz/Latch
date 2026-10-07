import { createPrivateKey, sign } from 'node:crypto';
import type { AdmissionClaims } from '../../admission-contract.ts';

export { REMOTE_LINK_LIMITS } from '../../admission-contract.ts';
export interface AdmissionPayload extends AdmissionClaims {
  readonly leaseId?: string;
}

/** Produces the compact Ed25519 admission/lease claim consumed by the relay. */
export function signAdmission(payload: AdmissionPayload, privateKeyPem: string): string {
  const header = Buffer.from(JSON.stringify({ alg: 'EdDSA', kid: payload.kid })).toString('base64url');
  const body = Buffer.from(JSON.stringify(payload)).toString('base64url');
  const signature = sign(null, Buffer.from(`${header}.${body}`), createPrivateKey(privateKeyPem));
  return `${header}.${body}.${signature.toString('base64url')}`;
}
