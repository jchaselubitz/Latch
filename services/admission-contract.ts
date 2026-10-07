/** Shared wire contract for control-plane admission tokens and relay claims. */
export type AdmissionRole = 'host' | 'controller';
export type AdmissionPurpose = 'enrollment' | 'session';

export interface AdmissionClaims {
  readonly iss: string;
  readonly aud: 'latch-relay';
  readonly kid: string;
  readonly roomId: string;
  readonly role: AdmissionRole;
  readonly purpose: AdmissionPurpose;
  readonly jti: string;
  readonly generation: number;
  readonly nbf: number;
  readonly exp: number;
  readonly limits: typeof REMOTE_LINK_LIMITS;
}

export const REMOTE_LINK_LIMITS = {
  maxStreams: 32,
  streamWindowBytes: 262144,
  totalBufferBytes: 8388608,
  recordBytes: 65535,
} as const;
