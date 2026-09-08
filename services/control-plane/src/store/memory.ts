/**
 * In-memory store.
 *
 * This is the reference implementation of the storage contract. Tests run the
 * real HTTP surface against it, which keeps the suite hermetic while the
 * PostgreSQL implementation is verified separately by the integration test
 * that runs when DATABASE_URL is present.
 */

import type {
  Account,
  AccessEvent,
  Device,
  Pairing,
  Permission,
  OwnerInvitation,
  PushRegistration,
  RemoteAdmission,
  RemoteEnrollment,
  RemoteLink,
  RelayRevocation,
} from '../domain.ts';
import { digestsMatch } from '../credentials.ts';
import type {
  CreateAccountInput,
  CreateDeviceInput,
  CreateOwnerInvitationInput,
  CreateRemoteAdmissionInput,
  ClaimRemoteEnrollmentInput,
  FinalizeRemoteEnrollmentInput,
  CreateRemoteEnrollmentInput,
  Store,
} from './types.ts';

const pairKey = (host: string, client: string): string => `${host}\u0000${client}`;

export class MemoryStore implements Store {
  #accounts = new Map<string, Account>();
  #accountDigests = new Map<string, string>();
  #devices = new Map<string, Device>();
  #deviceDigests = new Map<string, string>();
  #pairings = new Map<string, Pairing>();
  #events: AccessEvent[] = [];
  #ownerInvitations = new Map<string, OwnerInvitation>();
  #remoteEnrollments = new Map<string, RemoteEnrollment>();
  #remoteLinks = new Map<string, RemoteLink>();
  #remoteAdmissions = new Map<string, RemoteAdmission>();
  #relayRevocations = new Map<string, RelayRevocation>();
  #pushRegistrations = new Map<string, PushRegistration>();
  #attentionEvents = new Map<string, string>();

  /**
   * Full internal state, used by the privacy tests to assert that no
   * plaintext credential is ever retained.
   */
  snapshot(): Record<string, unknown> {
    return {
      accounts: [...this.#accounts.values()],
      accountDigests: [...this.#accountDigests.entries()],
      devices: [...this.#devices.values()],
      deviceDigests: [...this.#deviceDigests.entries()],
      pairings: [...this.#pairings.values()],
      events: this.#events,
      ownerInvitations: [...this.#ownerInvitations.values()],
      remoteEnrollments: [...this.#remoteEnrollments.values()],
      remoteLinks: [...this.#remoteLinks.values()],
      remoteAdmissions: [...this.#remoteAdmissions.values()],
      relayRevocations: [...this.#relayRevocations.values()],
      pushRegistrations: [...this.#pushRegistrations.values()],
      attentionEvents: [...this.#attentionEvents.entries()],
    };
  }

  async ping(): Promise<void> {}

  async close(): Promise<void> {}

  async createAccount(input: CreateAccountInput): Promise<Account> {
    const account: Account = {
      id: input.id,
      label: input.label,
      relayEnabled: true,
      createdAt: new Date().toISOString(),
    };
    this.#accounts.set(account.id, account);
    this.#accountDigests.set(account.id, input.tokenDigest);
    return account;
  }

  async getAccount(accountId: string): Promise<Account | null> {
    return this.#accounts.get(accountId) ?? null;
  }

  async setRelayEnabled(accountId: string, enabled: boolean): Promise<Account | null> {
    const account = this.#accounts.get(accountId);
    if (!account) {
      return null;
    }
    const updated: Account = { ...account, relayEnabled: enabled };
    this.#accounts.set(accountId, updated);
    if (!enabled) {
      for (const link of this.#remoteLinks.values()) {
        if (link.accountId === accountId) this.#enqueueRevocation(link.roomId);
      }
    }
    return updated;
  }

  async createOwnerInvitation(input: CreateOwnerInvitationInput): Promise<OwnerInvitation> {
    const invitation: OwnerInvitation = { ...input, consumedAt: null, createdAt: new Date().toISOString() };
    this.#ownerInvitations.set(input.id, invitation);
    return invitation;
  }

  async consumeOwnerInvitation(id: string, secretDigest: string, consumedAt: string, now: number): Promise<boolean> {
    const invitation = this.#ownerInvitations.get(id);
    if (!invitation || invitation.consumedAt || invitation.expiresAt <= now || !digestsMatch(invitation.secretDigest, secretDigest)) return false;
    this.#ownerInvitations.set(id, { ...invitation, consumedAt });
    return true;
  }

  async accountByTokenDigest(accountId: string, tokenDigest: string): Promise<Account | null> {
    const stored = this.#accountDigests.get(accountId);
    if (!stored || !digestsMatch(stored, tokenDigest)) {
      return null;
    }
    return this.#accounts.get(accountId) ?? null;
  }

  async createDevice(input: CreateDeviceInput): Promise<Device> {
    const device: Device = {
      id: input.id,
      accountId: input.accountId,
      name: input.name,
      platform: input.platform,
      role: input.role,
      publicKey: input.publicKey,
      keyGeneration: 1,
      revokedAt: null,
      lastSeenAt: null,
      createdAt: new Date().toISOString(),
    };
    this.#devices.set(device.id, device);
    this.#deviceDigests.set(device.id, input.tokenDigest);
    return device;
  }

  async getDevice(deviceId: string): Promise<Device | null> {
    return this.#devices.get(deviceId) ?? null;
  }

  async listDevices(accountId: string): Promise<Device[]> {
    return [...this.#devices.values()]
      .filter((device) => device.accountId === accountId)
      .sort((left, right) => left.createdAt.localeCompare(right.createdAt));
  }

  async countDevices(accountId: string): Promise<number> {
    return (await this.listDevices(accountId)).length;
  }

  async deviceByTokenDigest(deviceId: string, tokenDigest: string): Promise<Device | null> {
    const stored = this.#deviceDigests.get(deviceId);
    if (!stored || !digestsMatch(stored, tokenDigest)) {
      return null;
    }
    const device = this.#devices.get(deviceId);
    if (!device || device.revokedAt !== null) {
      return null;
    }
    return device;
  }

  async touchDevice(deviceId: string, seenAt: string): Promise<void> {
    const device = this.#devices.get(deviceId);
    if (device) {
      this.#devices.set(deviceId, { ...device, lastSeenAt: seenAt });
    }
  }

  async rotateDeviceKey(deviceId: string, publicKey: string): Promise<Device | null> {
    const device = this.#devices.get(deviceId);
    if (!device || device.revokedAt !== null) {
      return null;
    }
    const updated: Device = {
      ...device,
      publicKey,
      keyGeneration: device.keyGeneration + 1,
    };
    this.#devices.set(deviceId, updated);
    return updated;
  }

  async revokeDevice(deviceId: string, revokedAt: string): Promise<Device | null> {
    const device = this.#devices.get(deviceId);
    if (!device) {
      return null;
    }
    const updated: Device = { ...device, revokedAt: device.revokedAt ?? revokedAt };
    this.#devices.set(deviceId, updated);
    for (const [key, pairing] of this.#pairings) {
      if (
        pairing.revokedAt === null &&
        (pairing.hostDeviceId === deviceId || pairing.clientDeviceId === deviceId)
      ) {
        this.#pairings.set(key, { ...pairing, revokedAt });
      }
    }
    for (const link of this.#remoteLinks.values()) {
      if (link.hostDeviceId === deviceId || link.clientDeviceId === deviceId) this.#enqueueRevocation(link.roomId);
    }
    this.#pushRegistrations.delete(deviceId);
    return updated;
  }

  async upsertPairing(
    accountId: string,
    hostDeviceId: string,
    clientDeviceId: string,
    permission: Permission,
  ): Promise<Pairing> {
    const key = pairKey(hostDeviceId, clientDeviceId);
    const existing = this.#pairings.get(key);
    const pairing: Pairing = {
      accountId,
      hostDeviceId,
      clientDeviceId,
      permission,
      createdAt: existing?.createdAt ?? new Date().toISOString(),
      revokedAt: null,
    };
    this.#pairings.set(key, pairing);
    if (existing) {
      const link = this.#remoteLinks.get(key);
      if (link) this.#remoteLinks.set(key, {
        ...link,
        grantRevision: link.grantRevision + 1,
        hostGeneration: link.hostGeneration + 1,
        controllerGeneration: link.controllerGeneration + 1,
      });
    }
    return pairing;
  }

  async getPairing(hostDeviceId: string, clientDeviceId: string): Promise<Pairing | null> {
    const pairing = this.#pairings.get(pairKey(hostDeviceId, clientDeviceId));
    return pairing && pairing.revokedAt === null ? pairing : null;
  }

  async listPairingsForDevice(deviceId: string): Promise<Pairing[]> {
    return [...this.#pairings.values()].filter(
      (pairing) =>
        pairing.revokedAt === null &&
        (pairing.hostDeviceId === deviceId || pairing.clientDeviceId === deviceId),
    );
  }

  async revokePairing(
    hostDeviceId: string,
    clientDeviceId: string,
    revokedAt: string,
  ): Promise<boolean> {
    const key = pairKey(hostDeviceId, clientDeviceId);
    const pairing = this.#pairings.get(key);
    if (!pairing || pairing.revokedAt !== null) {
      return false;
    }
    this.#pairings.set(key, { ...pairing, revokedAt });
    const link = this.#remoteLinks.get(key);
    if (link) this.#enqueueRevocation(link.roomId);
    // An unpaired phone has no Mac left to be told about.
    this.#pushRegistrations.delete(clientDeviceId);
    return true;
  }

  async upsertPushRegistration(deviceId: string, pushToken: string, updatedAt: string): Promise<void> {
    this.#pushRegistrations.set(deviceId, { deviceId, pushToken, updatedAt });
  }

  async getPushRegistration(deviceId: string): Promise<PushRegistration | null> {
    return this.#pushRegistrations.get(deviceId) ?? null;
  }

  async deletePushRegistration(deviceId: string): Promise<boolean> {
    return this.#pushRegistrations.delete(deviceId);
  }

  async recordAttentionEvent(
    hostDeviceId: string,
    clientDeviceId: string,
    eventId: string,
    createdAt: string,
  ): Promise<boolean> {
    const key = `${hostDeviceId}\u0000${eventId}`;
    if (this.#attentionEvents.has(key)) return false;
    this.#attentionEvents.set(key, `${clientDeviceId}\u0000${createdAt}`);
    return true;
  }

  async createRemoteEnrollment(input: CreateRemoteEnrollmentInput): Promise<RemoteEnrollment> {
    const value: RemoteEnrollment = {
      ...input, provisionalDeviceId: null, provisionalName: null, provisionalPlatform: null,
      provisionalPublicKey: null, provisionalTokenDigest: null, completedAt: null, cancelledAt: null,
      createdAt: new Date().toISOString(),
    };
    this.#remoteEnrollments.set(input.id, value);
    return value;
  }

  async getRemoteEnrollment(id: string, now: number): Promise<RemoteEnrollment | null> {
    const value = this.#remoteEnrollments.get(id);
    return value && value.expiresAt > now && !value.cancelledAt ? value : null;
  }

  async claimRemoteEnrollment(input: ClaimRemoteEnrollmentInput): Promise<boolean> {
    const value = await this.getRemoteEnrollment(input.id, input.now);
    if (!value || value.provisionalDeviceId || !digestsMatch(value.admissionDigest, input.admissionDigest)) return false;
    this.#remoteEnrollments.set(input.id, {
      ...value,
      provisionalDeviceId: input.provisionalDeviceId,
      provisionalName: input.provisionalName,
      provisionalPlatform: input.provisionalPlatform,
      provisionalPublicKey: input.provisionalPublicKey,
      provisionalTokenDigest: input.provisionalTokenDigest,
    });
    return true;
  }

  async finalizeRemoteEnrollment(input: FinalizeRemoteEnrollmentInput): Promise<RemoteLink | null> {
    const value = await this.getRemoteEnrollment(input.id, input.now);
    if (!value || value.completedAt || value.hostDeviceId !== input.hostDeviceId ||
        !value.provisionalDeviceId || !value.provisionalName || !value.provisionalPlatform ||
        !value.provisionalTokenDigest || value.provisionalPublicKey !== input.controllerPublicKey ||
        (await this.countDevices(value.accountId)) >= input.maxDevices) return null;
    const device: Device = {
      id: value.provisionalDeviceId, accountId: value.accountId, name: value.provisionalName,
      platform: value.provisionalPlatform, role: 'client', publicKey: input.controllerPublicKey,
      keyGeneration: 1, revokedAt: null, lastSeenAt: null, createdAt: input.completedAt,
    };
    const pairing: Pairing = {
      accountId: value.accountId, hostDeviceId: value.hostDeviceId,
      clientDeviceId: device.id, permission: input.permission,
      createdAt: input.completedAt, revokedAt: null,
    };
    const link: RemoteLink = {
      id: input.linkId, accountId: value.accountId, hostDeviceId: value.hostDeviceId,
      clientDeviceId: device.id, roomId: input.linkRoomId, grantRevision: 1,
      hostGeneration: 0, controllerGeneration: 0, createdAt: input.completedAt,
    };
    this.#devices.set(device.id, device);
    this.#deviceDigests.set(device.id, value.provisionalTokenDigest);
    this.#pairings.set(pairKey(value.hostDeviceId, device.id), pairing);
    this.#remoteLinks.set(pairKey(value.hostDeviceId, device.id), link);
    this.#remoteEnrollments.set(input.id, { ...value, completedAt: input.completedAt });
    return link;
  }

  async cancelRemoteEnrollment(id: string, cancelledAt: string): Promise<boolean> {
    const value = this.#remoteEnrollments.get(id);
    if (!value || value.completedAt || value.cancelledAt) return false;
    this.#remoteEnrollments.set(id, { ...value, cancelledAt });
    this.#enqueueRevocation(value.roomId);
    return true;
  }

  async getOrCreateRemoteLink(accountId: string, hostDeviceId: string, clientDeviceId: string, roomId: string): Promise<RemoteLink> {
    const key = pairKey(hostDeviceId, clientDeviceId);
    const existing = this.#remoteLinks.get(key);
    if (existing) return existing;
    const value: RemoteLink = {
      id: `link_${roomId}`, accountId, hostDeviceId, clientDeviceId, roomId,
      grantRevision: 1, hostGeneration: 0, controllerGeneration: 0,
      createdAt: new Date().toISOString(),
    };
    this.#remoteLinks.set(key, value);
    return value;
  }

  async getRemoteLink(id: string): Promise<RemoteLink | null> {
    return [...this.#remoteLinks.values()].find((link) => link.id === id) ?? null;
  }

  async createRemoteAdmission(input: CreateRemoteAdmissionInput): Promise<RemoteAdmission> {
    const value: RemoteAdmission = {
      ...input, attemptId: null, leaseId: null, leaseExpiresAt: null,
      createdAt: new Date().toISOString(),
    };
    this.#remoteAdmissions.set(input.id, value);
    if (input.linkId) {
      for (const [key, link] of this.#remoteLinks) {
        if (link.id === input.linkId) {
          this.#remoteLinks.set(key, input.role === 'host'
            ? { ...link, hostGeneration: input.generation }
            : { ...link, controllerGeneration: input.generation });
        }
      }
    }
    return value;
  }

  async redeemRemoteAdmission(id: string, attemptId: string, leaseId: string, leaseExpiresAt: number, now: number): Promise<RemoteAdmission | null> {
    const value = this.#remoteAdmissions.get(id);
    if (!value || value.expiresAt <= now) return null;
    if (value.attemptId) return value.attemptId === attemptId ? value : null;
    const redeemed: RemoteAdmission = { ...value, attemptId, leaseId, leaseExpiresAt };
    this.#remoteAdmissions.set(id, redeemed);
    return redeemed;
  }

  async getRemoteAdmission(id: string): Promise<RemoteAdmission | null> {
    return this.#remoteAdmissions.get(id) ?? null;
  }

  async getRemoteAdmissionByLease(leaseId: string): Promise<RemoteAdmission | null> {
    return [...this.#remoteAdmissions.values()].find((value) => value.leaseId === leaseId) ?? null;
  }

  async extendRemoteLease(leaseId: string, expiresAt: number, now: number): Promise<RemoteAdmission | null> {
    for (const [id, value] of this.#remoteAdmissions) {
      if (value.leaseId === leaseId && (value.leaseExpiresAt ?? 0) > now) {
        const extended = { ...value, leaseExpiresAt: expiresAt };
        this.#remoteAdmissions.set(id, extended);
        return extended;
      }
    }
    return null;
  }

  async listPendingRelayRevocations(limit: number): Promise<RelayRevocation[]> {
    return [...this.#relayRevocations.values()].filter((value) => !value.acknowledgedAt).slice(0, limit);
  }

  async acknowledgeRelayRevocation(id: string, acknowledgedAt: string): Promise<void> {
    const value = this.#relayRevocations.get(id);
    if (value) this.#relayRevocations.set(id, { ...value, acknowledgedAt });
  }

  #enqueueRevocation(roomId: string): void {
    const id = `rev_${roomId}_${this.#relayRevocations.size + 1}`;
    this.#relayRevocations.set(id, {
      id, roomId, notAfter: Math.floor(Date.now() / 1000) + 600, attempts: 0,
      acknowledgedAt: null, createdAt: new Date().toISOString(),
    });
  }


  async recordAccessEvent(event: AccessEvent): Promise<void> {
    this.#events.push(event);
    if (this.#events.length > 1024) {
      this.#events.splice(0, this.#events.length - 1024);
    }
  }

  async listAccessEvents(accountId: string, limit: number): Promise<AccessEvent[]> {
    return this.#events
      .filter((event) => event.accountId === accountId)
      .slice(-limit)
      .reverse();
  }

  async purgeExpired(now: number): Promise<number> {
    let removed = 0;
    for (const [key, enrollment] of this.#remoteEnrollments) {
      if (enrollment.expiresAt <= now && enrollment.completedAt === null) {
        this.#remoteEnrollments.delete(key);
        removed += 1;
      }
    }
    for (const [key, admission] of this.#remoteAdmissions) {
      if (admission.expiresAt <= now && (admission.leaseExpiresAt ?? 0) <= now) {
        this.#remoteAdmissions.delete(key);
        removed += 1;
      }
    }
    for (const [key, value] of this.#attentionEvents) {
      const createdAt = Date.parse(value.split('\u0000')[1] ?? '') / 1000;
      if (Number.isFinite(createdAt) && createdAt + 24 * 60 * 60 <= now) {
        this.#attentionEvents.delete(key);
        removed += 1;
      }
    }
    return removed;
  }
}
