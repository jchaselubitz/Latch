import { createPublicKey, verify } from 'node:crypto';

export type RelayRole = 'host' | 'controller';
export type RoomPurpose = 'enrollment' | 'session';

export interface AdmissionClaim {
  readonly iss: string;
  readonly aud: 'latch-relay';
  readonly kid: string;
  readonly roomId: string;
  readonly role: RelayRole;
  readonly purpose: RoomPurpose;
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
}

export interface LeaseExtensionClaim extends AdmissionClaim {
  readonly leaseId: string;
}

const text = (value: string): Buffer => Buffer.from(value, 'base64url');

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('claim is not an object');
  }
  return value as Record<string, unknown>;
}

/** Verifies the compact Ed25519 claim and its privacy-preserving v1 shape. */
function verifiedObject(
  token: string,
  publicKeys: ReadonlyMap<string, string>,
  issuer: string,
): { claim: Record<string, unknown>; kid: string } {
  const parts = token.split('.');
  if (parts.length !== 3 || parts.some((part) => part.length === 0)) {
    throw new Error('malformed admission token');
  }
  const header = object(JSON.parse(text(parts[0]!).toString('utf8')));
  if (header.alg !== 'EdDSA' || typeof header.kid !== 'string') {
    throw new Error('unsupported admission signature');
  }
  const key = publicKeys.get(header.kid);
  if (!key || !verify(null, Buffer.from(`${parts[0]}.${parts[1]}`), createPublicKey(key), text(parts[2]!))) {
    throw new Error('invalid admission signature');
  }
  const claim = object(JSON.parse(text(parts[1]!).toString('utf8')));
  if (claim.iss !== issuer || claim.aud !== 'latch-relay' || claim.kid !== header.kid) {
    throw new Error('invalid admission issuer');
  }
  return { claim, kid: header.kid };
}

function validateCommon(claim: Record<string, unknown>, nowSeconds: number, maximumLifetime: number): void {
  if (
    !/^[A-Za-z0-9_-]{43}$/.test(String(claim.roomId)) ||
    !/^[A-Za-z0-9_-]{16,96}$/.test(String(claim.jti)) ||
    !['host', 'controller'].includes(String(claim.role)) ||
    !['enrollment', 'session'].includes(String(claim.purpose)) ||
    !Number.isSafeInteger(claim.generation) || Number(claim.generation) < 1 ||
    !Number.isSafeInteger(claim.nbf) || !Number.isSafeInteger(claim.exp) ||
    Number(claim.nbf) > nowSeconds + 5 || Number(claim.exp) <= nowSeconds ||
    Number(claim.exp) - Number(claim.nbf) > maximumLifetime
  ) throw new Error('invalid admission claim');
  const limits = object(claim.limits);
  if (
    limits.maxStreams !== 32 || limits.streamWindowBytes !== 262144 ||
    limits.totalBufferBytes !== 8388608 || limits.recordBytes !== 65535 ||
    Object.keys(limits).sort().join(',') !== 'maxStreams,recordBytes,streamWindowBytes,totalBufferBytes'
  ) throw new Error('unsupported admission limits');
}

export function verifyAdmissionClaim(
  token: string,
  publicKeys: ReadonlyMap<string, string>,
  issuer: string,
  nowSeconds: number,
): AdmissionClaim {
  const { claim } = verifiedObject(token, publicKeys, issuer);
  const exact = ['aud', 'exp', 'generation', 'iss', 'jti', 'kid', 'limits', 'nbf', 'purpose', 'role', 'roomId'];
  if (Object.keys(claim).sort().join(',') !== exact.join(',')) {
    throw new Error('unexpected admission fields');
  }
  validateCommon(claim, nowSeconds, 65);
  return claim as unknown as AdmissionClaim;
}

/** Verifies a renewal delivered over the authenticated WSS control channel. */
export function verifyLeaseExtensionClaim(
  token: string,
  publicKeys: ReadonlyMap<string, string>,
  issuer: string,
  nowSeconds: number,
): LeaseExtensionClaim {
  const { claim } = verifiedObject(token, publicKeys, issuer);
  const exact = ['aud', 'exp', 'generation', 'iss', 'jti', 'kid', 'leaseId', 'limits', 'nbf', 'purpose', 'role', 'roomId'];
  if (Object.keys(claim).sort().join(',') !== exact.join(',') ||
      !/^[A-Za-z0-9_-]{16,96}$/.test(String(claim.leaseId))) {
    throw new Error('unexpected lease extension fields');
  }
  validateCommon(claim, nowSeconds, 605);
  return claim as unknown as LeaseExtensionClaim;
}
