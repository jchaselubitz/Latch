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
  Candidate,
  Device,
  DeviceRole,
  Pairing,
  PairingRequest,
  Permission,
  Presence,
  RelayTicket,
  RendezvousOffer,
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

export interface PublishPresenceInput {
  readonly deviceId: string;
  readonly accountId: string;
  readonly candidates: readonly Candidate[];
  readonly expiresAt: number;
}

export interface CreateOfferInput {
  readonly id: string;
  readonly accountId: string;
  readonly requesterDeviceId: string;
  readonly targetDeviceId: string;
  readonly requestId: string;
  readonly candidates: readonly Candidate[];
  readonly expiresAt: number;
}

export interface CreatePairingRequestInput {
  readonly pairingId: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly secretDigest: string;
  readonly phrase: string | null;
  readonly permission: Permission;
  readonly expiresAt: number;
}

export interface CreateRelayTicketInput {
  readonly relayId: string;
  readonly accountId: string;
  readonly hostDeviceId: string;
  readonly clientDeviceId: string;
  readonly secretDigest: string;
  readonly expiresAt: number;
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

  createDevice(input: CreateDeviceInput): Promise<Device>;
  getDevice(deviceId: string): Promise<Device | null>;
  listDevices(accountId: string): Promise<Device[]>;
  countDevices(accountId: string): Promise<number>;
  /** Resolves a device bearer credential; revoked devices must not resolve. */
  deviceByTokenDigest(deviceId: string, tokenDigest: string): Promise<Device | null>;
  touchDevice(deviceId: string, seenAt: string): Promise<void>;
  rotateDeviceKey(deviceId: string, publicKey: string): Promise<Device | null>;
  /**
   * Revokes a device and removes every capability derived from it: presence,
   * pending rendezvous offers, and unexpired relay tickets.
   */
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

  createPairingRequest(input: CreatePairingRequestInput): Promise<PairingRequest>;
  /** Unconsumed, unexpired request, or `null`. */
  getPairingRequest(pairingId: string, now: number): Promise<PairingRequest | null>;
  /** Marks a request consumed. Returns false when another caller won the race. */
  consumePairingRequest(pairingId: string, consumedAt: string, now: number): Promise<boolean>;
  countPendingPairingRequests(hostDeviceId: string, now: number): Promise<number>;

  publishPresence(input: PublishPresenceInput): Promise<Presence>;
  getPresence(deviceId: string, now: number): Promise<Presence | null>;
  clearPresence(deviceId: string): Promise<void>;

  createOffer(input: CreateOfferInput): Promise<RendezvousOffer>;
  /** Returns and consumes the offers addressed to `deviceId`. */
  takeOffers(targetDeviceId: string, now: number): Promise<RendezvousOffer[]>;

  createRelayTicket(input: CreateRelayTicketInput): Promise<RelayTicket>;
  getRelayTicket(relayId: string, now: number): Promise<RelayTicket | null>;
  /**
   * Records a one-time endpoint admission. Returns `null` when the ticket is
   * unknown or expired and `false` when the device was already admitted.
   */
  admitToRelayTicket(relayId: string, deviceId: string, now: number): Promise<boolean | null>;

  recordAccessEvent(event: AccessEvent): Promise<void>;
  listAccessEvents(accountId: string, limit: number): Promise<AccessEvent[]>;

  /** Deletes expired presence, offers, and tickets. Returns rows removed. */
  purgeExpired(now: number): Promise<number>;
}
