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
  PairingRequest,
  Permission,
  Presence,
  RelayTicket,
  RendezvousOffer,
} from '../domain.ts';
import { digestsMatch } from '../credentials.ts';
import type {
  CreateAccountInput,
  CreateDeviceInput,
  CreateOfferInput,
  CreatePairingRequestInput,
  CreateRelayTicketInput,
  CreateTurnCredentialInput,
  PublishPresenceInput,
  Store,
} from './types.ts';

const pairKey = (host: string, client: string): string => `${host}\u0000${client}`;

export class MemoryStore implements Store {
  #accounts = new Map<string, Account>();
  #accountDigests = new Map<string, string>();
  #devices = new Map<string, Device>();
  #deviceDigests = new Map<string, string>();
  #pairings = new Map<string, Pairing>();
  #presence = new Map<string, Presence>();
  #offers = new Map<string, RendezvousOffer>();
  #tickets = new Map<string, RelayTicket>();
  #pairingRequests = new Map<string, PairingRequest>();
  #events: AccessEvent[] = [];
  #turnCredentials = new Map<string, CreateTurnCredentialInput>();

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
      pairingRequests: [...this.#pairingRequests.values()],
      presence: [...this.#presence.values()],
      offers: [...this.#offers.values()],
      tickets: [...this.#tickets.values()],
      turnCredentials: [...this.#turnCredentials.values()],
      events: this.#events,
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
    return updated;
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
    this.#presence.delete(deviceId);
    for (const [key, pairing] of this.#pairings) {
      if (
        pairing.revokedAt === null &&
        (pairing.hostDeviceId === deviceId || pairing.clientDeviceId === deviceId)
      ) {
        this.#pairings.set(key, { ...pairing, revokedAt });
      }
    }
    for (const [key, offer] of this.#offers) {
      if (offer.requesterDeviceId === deviceId || offer.targetDeviceId === deviceId) {
        this.#offers.delete(key);
      }
    }
    for (const [key, ticket] of this.#tickets) {
      if (ticket.hostDeviceId === deviceId || ticket.clientDeviceId === deviceId) {
        this.#tickets.delete(key);
      }
    }
    for (const [key, request] of this.#pairingRequests) {
      if (request.hostDeviceId === deviceId) {
        this.#pairingRequests.delete(key);
      }
    }
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
    for (const [ticketId, ticket] of this.#tickets) {
      if (ticket.hostDeviceId === hostDeviceId && ticket.clientDeviceId === clientDeviceId) {
        this.#tickets.delete(ticketId);
      }
    }
    return true;
  }

  async createPairingRequest(input: CreatePairingRequestInput): Promise<PairingRequest> {
    const request: PairingRequest = {
      ...input,
      consumedAt: null,
      createdAt: new Date().toISOString(),
    };
    this.#pairingRequests.set(request.pairingId, request);
    return request;
  }

  async getPairingRequest(pairingId: string, now: number): Promise<PairingRequest | null> {
    const request = this.#pairingRequests.get(pairingId);
    if (!request || request.consumedAt !== null || request.expiresAt <= now) {
      return null;
    }
    return request;
  }

  async consumePairingRequest(
    pairingId: string,
    consumedAt: string,
    now: number,
  ): Promise<boolean> {
    const request = await this.getPairingRequest(pairingId, now);
    if (!request) {
      return false;
    }
    this.#pairingRequests.set(pairingId, { ...request, consumedAt });
    return true;
  }

  async countPendingPairingRequests(hostDeviceId: string, now: number): Promise<number> {
    return [...this.#pairingRequests.values()].filter(
      (request) =>
        request.hostDeviceId === hostDeviceId &&
        request.consumedAt === null &&
        request.expiresAt > now,
    ).length;
  }

  async publishPresence(input: PublishPresenceInput): Promise<Presence> {
    const presence: Presence = {
      deviceId: input.deviceId,
      accountId: input.accountId,
      candidates: input.candidates,
      iceUfrag: input.iceUfrag,
      icePwd: input.icePwd,
      expiresAt: input.expiresAt,
      updatedAt: new Date().toISOString(),
    };
    this.#presence.set(presence.deviceId, presence);
    return presence;
  }

  async getPresence(deviceId: string, now: number): Promise<Presence | null> {
    const presence = this.#presence.get(deviceId);
    if (!presence || presence.expiresAt <= now) {
      return null;
    }
    return presence;
  }

  async clearPresence(deviceId: string): Promise<void> {
    this.#presence.delete(deviceId);
  }

  async createOffer(input: CreateOfferInput): Promise<RendezvousOffer> {
    const offer: RendezvousOffer = { ...input, createdAt: new Date().toISOString() };
    this.#offers.set(offer.id, offer);
    return offer;
  }

  async takeOffers(targetDeviceId: string, now: number): Promise<RendezvousOffer[]> {
    const taken: RendezvousOffer[] = [];
    for (const [key, offer] of this.#offers) {
      if (offer.expiresAt <= now) {
        this.#offers.delete(key);
        continue;
      }
      if (offer.targetDeviceId === targetDeviceId) {
        taken.push(offer);
        this.#offers.delete(key);
      }
    }
    return taken.sort((left, right) => left.createdAt.localeCompare(right.createdAt));
  }

  async createRelayTicket(input: CreateRelayTicketInput): Promise<RelayTicket> {
    const ticket: RelayTicket = {
      ...input,
      admittedDeviceIds: [],
      createdAt: new Date().toISOString(),
    };
    this.#tickets.set(ticket.relayId, ticket);
    return ticket;
  }

  async getRelayTicket(relayId: string, now: number): Promise<RelayTicket | null> {
    const ticket = this.#tickets.get(relayId);
    if (!ticket || ticket.expiresAt <= now) {
      return null;
    }
    return ticket;
  }

  async admitToRelayTicket(
    relayId: string,
    deviceId: string,
    now: number,
  ): Promise<boolean | null> {
    const ticket = await this.getRelayTicket(relayId, now);
    if (!ticket) {
      return null;
    }
    if (ticket.admittedDeviceIds.includes(deviceId)) {
      return false;
    }
    this.#tickets.set(relayId, {
      ...ticket,
      admittedDeviceIds: [...ticket.admittedDeviceIds, deviceId],
    });
    return true;
  }

  async createTurnCredential(input: CreateTurnCredentialInput): Promise<void> {
    this.#turnCredentials.set(input.username, input);
  }

  async takeTurnCredentialUsernames(deviceIds: readonly string[], now: number): Promise<string[]> {
    const ids = new Set(deviceIds);
    return this.#takeTurnCredentials((credential) => ids.has(credential.deviceId), now);
  }

  async takeTurnCredentialUsernamesForAccount(accountId: string, now: number): Promise<string[]> {
    return this.#takeTurnCredentials((credential) => credential.accountId === accountId, now);
  }

  #takeTurnCredentials(match: (credential: CreateTurnCredentialInput) => boolean, now: number): string[] {
    const usernames: string[] = [];
    for (const [username, credential] of this.#turnCredentials) {
      if (credential.expiresAt <= now || match(credential)) {
        this.#turnCredentials.delete(username);
        if (credential.expiresAt > now && match(credential)) usernames.push(username);
      }
    }
    return usernames;
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
    for (const [key, presence] of this.#presence) {
      if (presence.expiresAt <= now) {
        this.#presence.delete(key);
        removed += 1;
      }
    }
    for (const [key, offer] of this.#offers) {
      if (offer.expiresAt <= now) {
        this.#offers.delete(key);
        removed += 1;
      }
    }
    for (const [key, ticket] of this.#tickets) {
      if (ticket.expiresAt <= now) {
        this.#tickets.delete(key);
        removed += 1;
      }
    }
    for (const [key, request] of this.#pairingRequests) {
      if (request.expiresAt <= now) {
        this.#pairingRequests.delete(key);
        removed += 1;
      }
    }
    return removed;
  }
}
