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

/** Operator-minted, single-use bootstrap for the sole owner's account. */
export interface OwnerInvitation {
  readonly id: string;
  readonly secretDigest: string;
  readonly expiresAt: number;
  readonly consumedAt: string | null;
  readonly createdAt: string;
}

/** Provisional enrollment; its QR-only secret is deliberately absent. */
export interface RemoteEnrollment {
  readonly id: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly roomId: string;
  readonly admissionDigest: string;
  readonly expiresAt: number;
  readonly provisionalDeviceId: string | null;
  readonly provisionalName: string | null;
  readonly provisionalPlatform: string | null;
  readonly provisionalPublicKey: string | null;
  readonly provisionalTokenDigest: string | null;
  readonly completedAt: string | null;
  readonly cancelledAt: string | null;
  readonly createdAt: string;
}

/** Stable opaque room assignment for one locally approved pair. */
export interface RemoteLink {
  readonly id: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly clientDeviceId: string;
  readonly roomId: string;
  readonly grantRevision: number;
  readonly hostGeneration: number;
  readonly controllerGeneration: number;
  readonly createdAt: string;
}

/** Single-use signed relay admission tracked beyond its expiry. */
export interface RemoteAdmission {
  readonly id: string;
  readonly linkId: string | null;
  readonly enrollmentId: string | null;
  readonly roomId: string;
  readonly role: 'host' | 'controller';
  readonly purpose: 'enrollment' | 'session';
  readonly generation: number;
  readonly expiresAt: number;
  readonly attemptId: string | null;
  readonly leaseId: string | null;
  readonly leaseExpiresAt: number | null;
  readonly createdAt: string;
}

/** Durable room invalidation delivered to the relay at least once. */
export interface RelayRevocation {
  readonly id: string;
  readonly roomId: string;
  readonly notAfter: number;
  readonly attempts: number;
  readonly acknowledgedAt: string | null;
  readonly createdAt: string;
}

/** One opaque APNs token per controller device. */
export interface PushRegistration {
  readonly deviceId: string;
  readonly pushToken: string;
  readonly updatedAt: string;
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
