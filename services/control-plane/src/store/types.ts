/**
 * Storage boundary.
 *
 * The HTTP layer is written against this interface so the same request
 * handling is exercised by fast in-memory tests and by PostgreSQL in
 * production. Keeping it narrow also keeps the persisted surface auditable:
 * there is no method here that could accept terminal content.
 */

import type {
  Account,
  AccessEvent,
  Device,
  DeviceRole,
  Pairing,
  Permission,
  OwnerInvitation,
  PushRegistration,
  RemoteAdmission,
  RemoteEnrollment,
  RemoteLink,
  RelayRevocation,
} from '../domain.ts';

export interface CreateAccountInput {
  readonly id: string;
  readonly label: string;
  readonly tokenDigest: string;
}

export interface CreateDeviceInput {
  readonly id: string;
  readonly accountId: string;
  readonly name: string;
  readonly platform: string;
  readonly role: DeviceRole;
  readonly publicKey: string;
  readonly tokenDigest: string;
}

export interface CreateOwnerInvitationInput {
  readonly id: string;
  readonly secretDigest: string;
  readonly expiresAt: number;
}

export interface CreateRemoteEnrollmentInput {
  readonly id: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly roomId: string;
  readonly admissionDigest: string;
  readonly expiresAt: number;
}

export interface CreateRemoteAdmissionInput {
  readonly id: string;
  readonly linkId: string | null;
  readonly enrollmentId: string | null;
  readonly roomId: string;
  readonly role: 'host' | 'controller';
  readonly purpose: 'enrollment' | 'session';
  readonly generation: number;
  readonly expiresAt: number;
}

export interface ClaimRemoteEnrollmentInput {
  readonly id: string;
  readonly admissionDigest: string;
  readonly provisionalDeviceId: string;
  readonly provisionalName: string;
  readonly provisionalPlatform: string;
  readonly provisionalPublicKey: string;
  readonly provisionalTokenDigest: string;
  readonly now: number;
}

export interface FinalizeRemoteEnrollmentInput {
  readonly id: string;
  readonly hostDeviceId: string;
  readonly controllerPublicKey: string;
  readonly permission: Permission;
  readonly linkId: string;
  readonly linkRoomId: string;
  readonly completedAt: string;
  readonly now: number;
  readonly maxDevices: number;
}

export interface Store {
  /** Liveness/readiness probe for the backing storage. */
  ping(): Promise<void>;
  close(): Promise<void>;

  createAccount(input: CreateAccountInput): Promise<Account>;
  getAccount(accountId: string): Promise<Account | null>;
  setRelayEnabled(accountId: string, enabled: boolean): Promise<Account | null>;
  /** Resolves an account bearer credential to its account. */
  accountByTokenDigest(accountId: string, tokenDigest: string): Promise<Account | null>;
  createOwnerInvitation(input: CreateOwnerInvitationInput): Promise<OwnerInvitation>;
  consumeOwnerInvitation(id: string, secretDigest: string, consumedAt: string, now: number): Promise<boolean>;

  createDevice(input: CreateDeviceInput): Promise<Device>;
  getDevice(deviceId: string): Promise<Device | null>;
  listDevices(accountId: string): Promise<Device[]>;
  countDevices(accountId: string): Promise<number>;
  /** Resolves a device bearer credential; revoked devices must not resolve. */
  deviceByTokenDigest(deviceId: string, tokenDigest: string): Promise<Device | null>;
  touchDevice(deviceId: string, seenAt: string): Promise<void>;
  rotateDeviceKey(deviceId: string, publicKey: string): Promise<Device | null>;
  /** Revokes a device and invalidates every Remote Link derived from it. */
  revokeDevice(deviceId: string, revokedAt: string): Promise<Device | null>;

  upsertPairing(
    accountId: string,
    hostDeviceId: string,
    clientDeviceId: string,
    permission: Permission,
  ): Promise<Pairing>;
  getPairing(hostDeviceId: string, clientDeviceId: string): Promise<Pairing | null>;
  /** Active pairings that include `deviceId` on either side. */
  listPairingsForDevice(deviceId: string): Promise<Pairing[]>;
  revokePairing(hostDeviceId: string, clientDeviceId: string, revokedAt: string): Promise<boolean>;

  createRemoteEnrollment(input: CreateRemoteEnrollmentInput): Promise<RemoteEnrollment>;
  getRemoteEnrollment(id: string, now: number): Promise<RemoteEnrollment | null>;
  claimRemoteEnrollment(input: ClaimRemoteEnrollmentInput): Promise<boolean>;
  /** Atomically commits the provisional device, grant, link, and enrollment. */
  finalizeRemoteEnrollment(input: FinalizeRemoteEnrollmentInput): Promise<RemoteLink | null>;
  cancelRemoteEnrollment(id: string, cancelledAt: string): Promise<boolean>;

  getOrCreateRemoteLink(accountId: string, hostDeviceId: string, clientDeviceId: string, roomId: string): Promise<RemoteLink>;
  getRemoteLink(id: string): Promise<RemoteLink | null>;
  createRemoteAdmission(input: CreateRemoteAdmissionInput): Promise<RemoteAdmission>;
  redeemRemoteAdmission(id: string, attemptId: string, leaseId: string, leaseExpiresAt: number, now: number): Promise<RemoteAdmission | null>;
  getRemoteAdmission(id: string): Promise<RemoteAdmission | null>;
  getRemoteAdmissionByLease(leaseId: string): Promise<RemoteAdmission | null>;
  extendRemoteLease(leaseId: string, expiresAt: number, now: number): Promise<RemoteAdmission | null>;
  listPendingRelayRevocations(limit: number): Promise<RelayRevocation[]>;
  acknowledgeRelayRevocation(id: string, acknowledgedAt: string): Promise<void>;

  /** Replaces the device's APNs token. Revocation and unpairing remove it. */
  upsertPushRegistration(deviceId: string, pushToken: string, updatedAt: string): Promise<void>;
  getPushRegistration(deviceId: string): Promise<PushRegistration | null>;
  deletePushRegistration(deviceId: string): Promise<boolean>;
  /**
   * Records one attention event for deduplication. Returns false when the
   * host already submitted this event id.
   */
  recordAttentionEvent(
    hostDeviceId: string,
    clientDeviceId: string,
    eventId: string,
    createdAt: string,
  ): Promise<boolean>;

  recordAccessEvent(event: AccessEvent): Promise<void>;
  listAccessEvents(accountId: string, limit: number): Promise<AccessEvent[]>;

  /** Deletes expired enrollment/admission state. Returns rows removed. */
  purgeExpired(now: number): Promise<number>;
}
