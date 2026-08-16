/**
 * Domain types for the control plane.
 *
 * Every type here is deliberately content-free. The control plane stores
 * account membership, opaque device identities, public keys, revocation, and
 * short-lived connectivity metadata. It never stores terminal bytes,
 * transcripts, session names, prompt answers, or a Latch gateway token — the
 * boundary documented in docs/REMOTE_ACCESS_THREAT_MODEL.md.
 */

/** A device that hosts sessions (a Mac) or consumes them (a phone/web client). */
export type DeviceRole = 'host' | 'client';

/**
 * Permission granted to a paired client device. The control plane records the
 * grant so a client can discover it, but the host device remains the
 * enforcement point for every privileged operation.
 */
export type Permission = 'observe' | 'interact' | 'control';

export const PERMISSIONS: readonly Permission[] = ['observe', 'interact', 'control'];

export interface Account {
  readonly id: string;
  readonly label: string;
  readonly relayEnabled: boolean;
  readonly createdAt: string;
}

export interface Device {
  readonly id: string;
  readonly accountId: string;
  readonly name: string;
  readonly platform: string;
  readonly role: DeviceRole;
  /** Hex-encoded Noise static public key pinned during local pairing. */
  readonly publicKey: string;
  readonly keyGeneration: number;
  readonly revokedAt: string | null;
  readonly lastSeenAt: string | null;
  readonly createdAt: string;
}

/**
 * A pairing declared by a host device after the local QR confirmation on the
 * unlocked Mac. The control plane never creates a pairing on its own: it can
 * only mirror one the host already approved, so a compromised control plane
 * cannot synthesize a device grant.
 */
export interface Pairing {
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly clientDeviceId: string;
  readonly permission: Permission;
  readonly createdAt: string;
  readonly revokedAt: string | null;
}

/** A short-lived, structured ICE endpoint candidate. Never a gateway address. */
export interface Candidate {
  readonly address: string;
  readonly expiresAt: number;
  /** RFC 8445 candidate type. Omitted only by older host-candidate publishers. */
  readonly type?: 'host' | 'srflx' | 'prflx' | 'relay';
  /** ICE candidate priority. */
  readonly priority?: number;
  /** ICE candidate foundation, constrained to transport metadata characters. */
  readonly foundation?: string;
  readonly component?: 1 | 2;
  readonly protocol?: 'udp' | 'tcp';
  /** Related endpoint for server-reflexive, peer-reflexive, and relay candidates. */
  readonly relatedAddress?: string;
  readonly relatedPort?: number;
  readonly tcpType?: 'active' | 'passive' | 'so';
}

export interface Presence {
  readonly deviceId: string;
  readonly accountId: string;
  readonly candidates: readonly Candidate[];
  /** ICE credentials are short-lived transport metadata, not a peer identity. */
  readonly iceUfrag?: string;
  readonly icePwd?: string;
  readonly expiresAt: number;
  readonly updatedAt: string;
}

/** A rendezvous offer held for the target device until it expires. */
export interface RendezvousOffer {
  readonly id: string;
  readonly accountId: string;
  readonly requesterDeviceId: string;
  readonly targetDeviceId: string;
  readonly requestId: string;
  readonly candidates: readonly Candidate[];
  readonly iceUfrag?: string;
  readonly icePwd?: string;
  readonly expiresAt: number;
  readonly createdAt: string;
}

/**
 * Relay admission material. The plaintext authentication secret is returned
 * once at issuance and only its digest is retained, so a control-plane
 * database leak cannot admit an endpoint to a live relay slot.
 */
export interface RelayTicket {
  readonly relayId: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly clientDeviceId: string;
  readonly secretDigest: string;
  readonly expiresAt: number;
  readonly admittedDeviceIds: readonly string[];
  readonly createdAt: string;
}

/**
 * A pairing request the host device is displaying as a QR code. Only the
 * digest of the one-time secret is held, mirroring the local Mac record, and
 * the row is single-use.
 */
export interface PairingRequest {
  readonly pairingId: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly secretDigest: string;
  readonly phrase: string | null;
  readonly permission: Permission;
  readonly expiresAt: number;
  readonly consumedAt: string | null;
  readonly createdAt: string;
}

/** Coarse, content-free audit record. */
export interface AccessEvent {
  readonly accountId: string | null;
  readonly deviceId: string | null;
  readonly action: string;
  readonly result: 'allowed' | 'denied';
  readonly createdAt: string;
}

/** Highest-permission-last ordering used for permission comparisons. */
export function permits(granted: Permission, required: Permission): boolean {
  return PERMISSIONS.indexOf(granted) >= PERMISSIONS.indexOf(required);
}
