/**
 * The Latch control-plane HTTP API.
 *
 * Responsibilities, and their limits:
 *
 * - account and device registration, and device revocation;
 * - a paired-device directory that mirrors grants a host device approved
 *   locally, never grants the control plane invents;
 * - short-lived presence and rendezvous so two paired devices can find each
 *   other's connection candidates;
 * - relay-ticket issuance and the authorization call the separately deployed
 *   relay makes before admitting an endpoint.
 *
 * What never passes through here: terminal bytes, transcripts, session names,
 * prompt answers, device private keys, and the Latch gateway bearer token.
 * End-to-end encryption is unaffected by this service: it coordinates
 * candidates and admission only, and both endpoints still verify the peer
 * static key pinned during local pairing.
 */

import type { Config } from './config.ts';
import type { TurnProvider } from './cloudflare-turn.ts';
import type { Device, Permission } from './domain.ts';
import { bearerToken, digestOf, issueCredential, newId, newSecret, subjectOf } from './credentials.ts';
import { HttpError, Router } from './http/router.ts';
import type { Handler, RequestContext } from './http/router.ts';
import { RateLimiter } from './rate-limit.ts';
import type { Store } from './store/types.ts';
import * as validate from './validation.ts';
import { ValidationError } from './validation.ts';

export interface ApiDependencies {
  readonly config: Config;
  readonly store: Store;
  /** Injected so tests can drive expiry deterministically. */
  readonly now: () => number;
  /** Names of applied migrations, for readiness reporting. */
  readonly readiness: () => Promise<{ migrations: string[] }>;
  readonly turn: TurnProvider | null;
}

const unauthorized = (): HttpError =>
  new HttpError(401, 'unauthorized', 'a valid bearer credential is required');

const forbidden = (message: string): HttpError => new HttpError(403, 'forbidden', message);

/** Mirrors the local Mac limits in crates/latch/src/cli/remote_access.rs. */
const MAX_PENDING_PAIRINGS = 8;
const PAIRING_TTL_SECONDS = 5 * 60;
/** Confirmation words shown on both screens. Not a credential. */
const PHRASE = /^[a-z]+(?:[ -][a-z]+){1,5}$/;

function unixSeconds(now: () => number): number {
  return Math.floor(now() / 1000);
}

function deviceView(device: Device, permission: Permission | null): Record<string, unknown> {
  return {
    deviceId: device.id,
    name: device.name,
    platform: device.platform,
    role: device.role,
    publicKey: device.publicKey,
    keyGeneration: device.keyGeneration,
    revoked: device.revokedAt !== null,
    lastSeenAt: device.lastSeenAt,
    permission,
  };
}

export function createRouter(dependencies: ApiDependencies): Router {
  const { config, store, now, turn } = dependencies;
  const limiter = new RateLimiter(config.rateLimitPerMinute, 60_000, now);

  /** Authenticates an account credential (`Authorization: Bearer <token>`). */
  async function requireAccount(context: RequestContext) {
    const token = bearerToken(context.headers.authorization);
    const subject = token ? subjectOf(token) : null;
    if (!token || !subject || !validate.isOpaqueId(subject)) {
      throw unauthorized();
    }
    const account = await store.accountByTokenDigest(subject, digestOf('account', token));
    if (!account) {
      throw unauthorized();
    }
    return account;
  }

  /**
   * Authenticates a device credential. A revoked device never resolves, so
   * revocation takes effect on the very next request rather than at expiry.
   */
  async function requireDevice(context: RequestContext): Promise<Device> {
    const token = bearerToken(context.headers.authorization);
    const subject = token ? subjectOf(token) : null;
    if (!token || !subject || !validate.isOpaqueId(subject)) {
      throw unauthorized();
    }
    const device = await store.deviceByTokenDigest(subject, digestOf('device', token));
    if (!device) {
      throw unauthorized();
    }
    if (!limiter.allow(device.id)) {
      throw new HttpError(429, 'rate_limited', 'too many requests for this device');
    }
    await store.touchDevice(device.id, new Date(now()).toISOString());
    return device;
  }

  /** Resolves the pairing between two devices regardless of which side asks. */
  async function pairingBetween(caller: Device, peer: Device) {
    if (caller.accountId !== peer.accountId) {
      return null;
    }
    return caller.role === 'host'
      ? store.getPairing(caller.id, peer.id)
      : store.getPairing(peer.id, caller.id);
  }

  async function audit(
    accountId: string | null,
    deviceId: string | null,
    action: string,
    result: 'allowed' | 'denied',
  ): Promise<void> {
    await store.recordAccessEvent({
      accountId,
      deviceId,
      action,
      result,
      createdAt: new Date(now()).toISOString(),
    });
  }

  async function revokeTurnCredentials(usernames: readonly string[]): Promise<void> {
    if (!turn || usernames.length === 0) return;
    // Cloudflare revocation is idempotent enough for retries; issue all calls
    // concurrently so local revocation does not leave a long exposure window.
    await Promise.all(usernames.map((username) => turn.revoke(username)));
  }

  const body = (context: RequestContext, allowed: readonly string[]) =>
    validate.object(context.body ?? {}, allowed);

  const router = new Router(withValidationMapping);

  // --- Health ---------------------------------------------------------------

  router.get('/health/live', async () => ({
    status: 200,
    body: { status: 'live', service: 'latch-control-plane', release: config.releaseId },
  }));

  router.get('/health/ready', async () => {
    try {
      await store.ping();
      const { migrations } = await dependencies.readiness();
      return {
        status: 200,
        body: {
          status: 'ready',
          service: 'latch-control-plane',
          release: config.releaseId,
          environment: config.environment,
          migrations: migrations.length,
          relayConfigured: Boolean(config.cloudflareTurnKeyId),
        },
      };
    } catch {
      throw new HttpError(503, 'not_ready', 'storage is unavailable');
    }
  });

  router.get('/health', async () => ({
    status: 200,
    body: { status: 'ok', service: 'latch-control-plane', release: config.releaseId },
  }));

  // --- Accounts -------------------------------------------------------------

  router.post('/v1/accounts', async (context) => {
    const input = body(context, ['label']);
    const accountId = newId('acct');
    const credential = issueCredential('account', accountId);
    const account = await store.createAccount({
      id: accountId,
      label: validate.label(input, 'label', 'Latch account'),
      tokenDigest: credential.digest,
    });
    await audit(account.id, null, 'account.create', 'allowed');
    return {
      status: 201,
      body: {
        accountId: account.id,
        label: account.label,
        relayEnabled: account.relayEnabled,
        // Returned exactly once; only its digest is stored.
        accountToken: credential.token,
      },
    };
  });

  router.get('/v1/account', async (context) => {
    const account = await requireAccount(context);
    return {
      status: 200,
      body: { accountId: account.id, label: account.label, relayEnabled: account.relayEnabled },
    };
  });

  /** The account-level equivalent of `latch remote-access relay disable`. */
  router.patch('/v1/account', async (context) => {
    const account = await requireAccount(context);
    const input = body(context, ['relayEnabled']);
    const enabled = validate.boolean(input, 'relayEnabled');
    const updated = await store.setRelayEnabled(account.id, enabled);
    if (!enabled) {
      const usernames = await store.takeTurnCredentialUsernamesForAccount(account.id, unixSeconds(now));
      await revokeTurnCredentials(usernames);
    }
    await audit(account.id, null, 'account.relay_enabled', 'allowed');
    return {
      status: 200,
      body: { accountId: account.id, relayEnabled: updated?.relayEnabled ?? account.relayEnabled },
    };
  });

  router.get('/v1/account/events', async (context) => {
    const account = await requireAccount(context);
    const events = await store.listAccessEvents(account.id, 100);
    return { status: 200, body: { events } };
  });

  // --- Device registration and revocation -----------------------------------

  router.post('/v1/devices', async (context) => {
    const account = await requireAccount(context);
    const input = body(context, ['name', 'platform', 'role', 'publicKey']);
    const role = validate.requiredString(input, 'role', /^(host|client)$/);
    const enrolled = await store.countDevices(account.id);
    if (enrolled >= config.maxDevicesPerAccount) {
      await audit(account.id, null, 'device.register', 'denied');
      throw forbidden('device limit reached for this account');
    }
    const deviceId = newId('dev');
    const credential = issueCredential('device', deviceId);
    const device = await store.createDevice({
      id: deviceId,
      accountId: account.id,
      name: validate.requiredLabel(input, 'name'),
      platform: validate.requiredString(input, 'platform', /^[a-z0-9.-]{2,32}$/),
      role: role as 'host' | 'client',
      publicKey: validate.publicKey(input, 'publicKey'),
      tokenDigest: credential.digest,
    });
    await audit(account.id, device.id, 'device.register', 'allowed');
    return {
      status: 201,
      body: {
        ...deviceView(device, null),
        // Returned exactly once; only its digest is stored.
        deviceToken: credential.token,
      },
    };
  });

  /** The paired-device directory as seen by the calling device. */
  router.get('/v1/devices', async (context) => {
    const caller = await requireDevice(context);
    const pairings = await store.listPairingsForDevice(caller.id);
    const permissions = new Map<string, Permission>();
    for (const pairing of pairings) {
      const peerId = pairing.hostDeviceId === caller.id ? pairing.clientDeviceId : pairing.hostDeviceId;
      permissions.set(peerId, pairing.permission);
    }
    const devices = await store.listDevices(caller.accountId);
    const visible = devices.filter(
      (device) => device.id === caller.id || permissions.has(device.id),
    );
    const nowSeconds = unixSeconds(now);
    const entries = [];
    for (const device of visible) {
      const online = (await store.getPresence(device.id, nowSeconds)) !== null;
      entries.push({
        ...deviceView(device, permissions.get(device.id) ?? null),
        self: device.id === caller.id,
        online,
      });
    }
    return { status: 200, body: { devices: entries } };
  });

  router.post('/v1/devices/:deviceId/rotate-key', async (context) => {
    const caller = await requireDevice(context);
    const deviceId = context.params.deviceId ?? '';
    const input = body(context, ['publicKey']);
    const target = await store.getDevice(deviceId);
    if (!target || target.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    // A device may rotate its own key; a host may rotate a paired client's key
    // after re-confirming it locally. A revoked device can never be rotated
    // back into service, so rotation is not a recovery path.
    const selfRotation = target.id === caller.id;
    const hostRotation = caller.role === 'host' && (await pairingBetween(caller, target)) !== null;
    if (!selfRotation && !hostRotation) {
      await audit(caller.accountId, target.id, 'device.rotate_key', 'denied');
      throw forbidden('only the device itself or its paired host may rotate this key');
    }
    const rotated = await store.rotateDeviceKey(target.id, validate.publicKey(input, 'publicKey'));
    if (!rotated) {
      throw forbidden('a revoked device cannot rotate its key');
    }
    await audit(caller.accountId, target.id, 'device.rotate_key', 'allowed');
    return { status: 200, body: deviceView(rotated, null) };
  });

  /**
   * Revocation. Presence, pending rendezvous offers, and unexpired relay
   * tickets for the device are dropped in the same operation, so a revoked
   * device loses discovery and relay admission immediately rather than at the
   * end of its current TTL.
   */
  router.post('/v1/devices/:deviceId/revoke', async (context) => {
    const deviceId = context.params.deviceId ?? '';
    // Either the account credential (the operator's incident switch) or a
    // device credential (the host that owns the pairing, or the device
    // retiring itself) may revoke.
    const account = await requireAccount(context).catch((error) => {
      if (error instanceof HttpError && error.status === 401) {
        return null;
      }
      throw error;
    });

    const target = await store.getDevice(deviceId);
    if (!target) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    if (account) {
      if (account.id !== target.accountId) {
        throw new HttpError(404, 'not_found', 'no such device');
      }
    } else {
      const caller = await requireDevice(context);
      if (caller.accountId !== target.accountId) {
        throw new HttpError(404, 'not_found', 'no such device');
      }
      const paired = caller.role === 'host' ? await pairingBetween(caller, target) : null;
      if (caller.id !== target.id && !paired) {
        await audit(caller.accountId, target.id, 'device.revoke', 'denied');
        throw forbidden('only the account, the device itself, or its paired host may revoke it');
      }
    }
    const revoked = await store.revokeDevice(target.id, new Date(now()).toISOString());
    const usernames = await store.takeTurnCredentialUsernames([target.id], unixSeconds(now));
    await revokeTurnCredentials(usernames);
    await audit(target.accountId, target.id, 'device.revoke', 'allowed');
    return { status: 200, body: deviceView(revoked ?? target, null) };
  });

  // --- Pairing directory ----------------------------------------------------

  /**
   * Records a pairing the host device already approved locally. Only a host
   * may declare one: the control plane mirrors local authorization, it does
   * not become an authority for it.
   */
  router.post('/v1/pairings', async (context) => {
    const caller = await requireDevice(context);
    const input = body(context, ['clientDeviceId', 'permission']);
    if (caller.role !== 'host') {
      await audit(caller.accountId, caller.id, 'pairing.create', 'denied');
      throw forbidden('only a host device may declare a pairing');
    }
    const clientDeviceId = validate.opaqueId(input, 'clientDeviceId');
    const client = await store.getDevice(clientDeviceId);
    if (!client || client.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    if (client.id === caller.id || client.role !== 'client') {
      throw new HttpError(409, 'invalid_pairing', 'a host can only pair with a client device');
    }
    if (client.revokedAt !== null) {
      await audit(caller.accountId, client.id, 'pairing.create', 'denied');
      throw forbidden('a revoked device cannot be paired');
    }
    const pairing = await store.upsertPairing(
      caller.accountId,
      caller.id,
      client.id,
      validate.permission(input, 'permission', 'interact'),
    );
    await audit(caller.accountId, client.id, 'pairing.create', 'allowed');
    return { status: 201, body: pairing };
  });

  /**
   * Registers a pairing the host is displaying as a QR code. The Mac keeps
   * the secret; the service receives only its digest, so a control-plane
   * breach cannot answer a scan on the Mac's behalf.
   */
  router.post('/v1/pairings/requests', async (context) => {
    const caller = await requireDevice(context);
    if (caller.role !== 'host') {
      throw forbidden('only a host device may open a pairing request');
    }
    const input = body(context, ['pairingId', 'secretDigest', 'phrase', 'permission', 'expiresAt']);
    const nowSeconds = unixSeconds(now);
    const pending = await store.countPendingPairingRequests(caller.id, nowSeconds);
    if (pending >= MAX_PENDING_PAIRINGS) {
      await audit(caller.accountId, caller.id, 'pairing.request', 'denied');
      throw forbidden('too many pending pairing requests');
    }
    const request = await store.createPairingRequest({
      pairingId: validate.requiredString(input, 'pairingId', /^[0-9a-zA-Z_-]{8,64}$/),
      accountId: caller.accountId,
      hostDeviceId: caller.id,
      secretDigest: validate.requiredString(input, 'secretDigest', /^[0-9a-f]{64}$/),
      phrase: input.phrase === undefined ? null : validate.requiredString(input, 'phrase', PHRASE),
      permission: validate.permission(input, 'permission', 'interact'),
      expiresAt: validate.expiresAt(input, 'expiresAt', nowSeconds, PAIRING_TTL_SECONDS),
    });
    await audit(caller.accountId, caller.id, 'pairing.request', 'allowed');
    return {
      status: 201,
      body: { pairingId: request.pairingId, expiresAt: request.expiresAt, phrase: request.phrase },
    };
  });

  /**
   * Confirms a scanned pairing. The one-time secret is the only credential:
   * possession of it proves the caller was in front of the unlocked Mac while
   * the code was displayed. The request is consumed before the device is
   * created, so two phones scanning the same code cannot both enrol.
   */
  router.post('/v1/pairings/:pairingId/confirm', async (context) => {
    const pairingId = context.params.pairingId ?? '';
    const input = body(context, ['formatVersion', 'secret', 'device', 'phrase']);
    const nowSeconds = unixSeconds(now);
    const request = await store.getPairingRequest(pairingId, nowSeconds);
    if (!request) {
      // Unknown, consumed, and expired are reported distinctly enough for the
      // client to say "show a new code" without revealing which it was.
      throw new HttpError(404, 'pairing_unavailable', 'no such pairing request');
    }
    if (digestOf('pairing', validate.requiredString(input, 'secret', /^[0-9a-zA-Z_-]{16,128}$/)) !==
      request.secretDigest) {
      await audit(request.accountId, request.hostDeviceId, 'pairing.confirm', 'denied');
      throw forbidden('pairing secret does not match');
    }
    const device = validate.object(input.device ?? {}, ['publicKey', 'name', 'platform']);
    const publicKey = validate.publicKey(device, 'publicKey');
    const host = await store.getDevice(request.hostDeviceId);
    if (!host || host.revokedAt !== null) {
      throw new HttpError(410, 'pairing_expired', 'the host device is no longer available');
    }
    const existing = (await store.listDevices(request.accountId)).find(
      (candidate) => candidate.publicKey === publicKey && candidate.revokedAt === null,
    );
    if (existing) {
      await audit(request.accountId, existing.id, 'pairing.confirm', 'denied');
      throw new HttpError(409, 'already_paired', 'this identity is already enrolled');
    }
    if ((await store.countDevices(request.accountId)) >= config.maxDevicesPerAccount) {
      throw forbidden('device limit reached for this account');
    }
    if (!(await store.consumePairingRequest(pairingId, new Date(now()).toISOString(), nowSeconds))) {
      throw new HttpError(409, 'pairing_consumed', 'this pairing code was already used');
    }
    const deviceId = newId('dev');
    const credential = issueCredential('device', deviceId);
    const enrolled = await store.createDevice({
      id: deviceId,
      accountId: request.accountId,
      name: validate.requiredLabel(device, 'name'),
      platform: validate.requiredString(device, 'platform', /^[a-z0-9.-]{2,32}$/),
      role: 'client',
      publicKey,
      tokenDigest: credential.digest,
    });
    const pairing = await store.upsertPairing(
      request.accountId,
      host.id,
      enrolled.id,
      request.permission,
    );
    await audit(request.accountId, enrolled.id, 'pairing.confirm', 'allowed');
    return {
      status: 201,
      body: {
        device: {
          deviceId: enrolled.id,
          name: enrolled.name,
          permission: pairing.permission,
          revoked: false,
        },
        mac: { deviceId: host.id, publicKey: host.publicKey, name: host.name },
        // Returned exactly once; only its digest is stored.
        accessToken: credential.token,
        phrase: request.phrase,
      },
    };
  });

  /** A device reads its own record and its host, to detect revocation. */
  router.get('/v1/devices/:deviceId', async (context) => {
    const caller = await requireDevice(context);
    if ((context.params.deviceId ?? '') !== caller.id) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    const pairings = await store.listPairingsForDevice(caller.id);
    const pairing = pairings.find((entry) => entry.clientDeviceId === caller.id) ?? null;
    const host = pairing ? await store.getDevice(pairing.hostDeviceId) : null;
    return {
      status: 200,
      body: {
        device: {
          deviceId: caller.id,
          name: caller.name,
          permission: pairing?.permission ?? 'observe',
          revoked: caller.revokedAt !== null,
        },
        mac: host
          ? { deviceId: host.id, publicKey: host.publicKey, name: host.name }
          : null,
      },
    };
  });

  router.get('/v1/pairings', async (context) => {
    const caller = await requireDevice(context);
    const pairings = await store.listPairingsForDevice(caller.id);
    return { status: 200, body: { pairings } };
  });

  router.delete('/v1/pairings/:peerDeviceId', async (context) => {
    const caller = await requireDevice(context);
    const peerId = context.params.peerDeviceId ?? '';
    const peer = await store.getDevice(peerId);
    if (!peer || peer.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    const host = caller.role === 'host' ? caller.id : peer.id;
    const client = caller.role === 'host' ? peer.id : caller.id;
    const removed = await store.revokePairing(host, client, new Date(now()).toISOString());
    if (!removed) {
      throw new HttpError(404, 'not_found', 'no such pairing');
    }
    const usernames = await store.takeTurnCredentialUsernames([host, client], unixSeconds(now));
    await revokeTurnCredentials(usernames);
    await audit(caller.accountId, peer.id, 'pairing.revoke', 'allowed');
    return { status: 200, body: { hostDeviceId: host, clientDeviceId: client, revoked: true } };
  });

  // --- Presence -------------------------------------------------------------

  router.post('/v1/presence', async (context) => {
    const caller = await requireDevice(context);
    const nowSeconds = unixSeconds(now);
    const input = body(context, ['candidates']);
    const parsed = validate.candidates(input, 'candidates', {
      max: config.maxCandidates,
      now: nowSeconds,
      maxLifetimeSeconds: config.presenceTtlSeconds,
    });
    const expiresAt = Math.min(
      nowSeconds + config.presenceTtlSeconds,
      Math.max(...parsed.map((candidate) => candidate.expiresAt)),
    );
    const presence = await store.publishPresence({
      deviceId: caller.id,
      accountId: caller.accountId,
      candidates: parsed,
      expiresAt,
    });
    return {
      status: 200,
      body: {
        deviceId: presence.deviceId,
        expiresAt: presence.expiresAt,
        ttlSeconds: presence.expiresAt - nowSeconds,
      },
    };
  });

  router.delete('/v1/presence', async (context) => {
    const caller = await requireDevice(context);
    await store.clearPresence(caller.id);
    return { status: 200, body: { deviceId: caller.id, published: false } };
  });

  router.get('/v1/presence/:deviceId', async (context) => {
    const caller = await requireDevice(context);
    const target = await store.getDevice(context.params.deviceId ?? '');
    if (!target || target.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    if (target.id !== caller.id && !(await pairingBetween(caller, target))) {
      await audit(caller.accountId, target.id, 'presence.read', 'denied');
      throw forbidden('devices must be paired to observe presence');
    }
    const presence = await store.getPresence(target.id, unixSeconds(now));
    if (!presence) {
      return { status: 200, body: { deviceId: target.id, online: false, candidates: [] } };
    }
    return {
      status: 200,
      body: {
        deviceId: target.id,
        online: true,
        identityKey: target.publicKey,
        candidates: presence.candidates,
        expiresAt: presence.expiresAt,
      },
    };
  });

  // --- Rendezvous -----------------------------------------------------------

  /**
   * Exchanges connection candidates with a paired, currently-present device.
   * The requester's candidates are held for the target to collect; the
   * response carries the target's candidates and its pinned identity key so
   * the caller can verify the peer static key during the Noise handshake.
   */
  router.post('/v1/rendezvous', async (context) => {
    const caller = await requireDevice(context);
    const nowSeconds = unixSeconds(now);
    const input = body(context, ['targetDeviceId', 'requestId', 'candidates', 'expiresAt']);
    const targetDeviceId = validate.opaqueId(input, 'targetDeviceId');
    if (targetDeviceId === caller.id) {
      throw new HttpError(409, 'invalid_target', 'rendezvous cannot target the calling device');
    }
    const target = await store.getDevice(targetDeviceId);
    if (!target || target.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    const pairing = await pairingBetween(caller, target);
    if (!pairing || target.revokedAt !== null) {
      await audit(caller.accountId, target.id, 'rendezvous.request', 'denied');
      throw forbidden('rendezvous requires an active pairing');
    }
    const requestId = validate.requestId(input, 'requestId');
    const parsed = validate.candidates(input, 'candidates', {
      max: config.maxCandidates,
      now: nowSeconds,
      maxLifetimeSeconds: config.rendezvousTtlSeconds,
    });
    const requestedExpiry = validate.expiresAt(
      input,
      'expiresAt',
      nowSeconds,
      config.rendezvousTtlSeconds,
    );
    const presence = await store.getPresence(target.id, nowSeconds);
    if (!presence) {
      await audit(caller.accountId, target.id, 'rendezvous.request', 'denied');
      throw new HttpError(409, 'target_offline', 'target device has no current presence');
    }
    await store.createOffer({
      id: newId('rdv'),
      accountId: caller.accountId,
      requesterDeviceId: caller.id,
      targetDeviceId: target.id,
      requestId,
      candidates: parsed,
      expiresAt: requestedExpiry,
    });
    await audit(caller.accountId, target.id, 'rendezvous.request', 'allowed');
    return {
      status: 200,
      body: {
        requestId,
        peerDeviceId: target.id,
        peerIdentityKey: target.publicKey,
        permission: pairing.permission,
        candidates: presence.candidates,
        expiresAt: Math.min(presence.expiresAt, requestedExpiry),
      },
    };
  });

  /** Collects and consumes the offers addressed to the calling device. */
  router.get('/v1/rendezvous', async (context) => {
    const caller = await requireDevice(context);
    const offers = await store.takeOffers(caller.id, unixSeconds(now));
    const results = [];
    for (const offer of offers) {
      const requester = await store.getDevice(offer.requesterDeviceId);
      if (!requester || requester.revokedAt !== null) {
        continue;
      }
      if (!(await pairingBetween(caller, requester))) {
        continue;
      }
      results.push({
        requestId: offer.requestId,
        peerDeviceId: requester.id,
        peerIdentityKey: requester.publicKey,
        candidates: offer.candidates,
        expiresAt: offer.expiresAt,
      });
    }
    return { status: 200, body: { offers: results } };
  });

  // --- Cloudflare Realtime TURN --------------------------------------------

  /** Issues Cloudflare ICE configuration only after existing pairing checks. */
  router.post('/v1/turn-credentials', async (context) => {
    const caller = await requireDevice(context);
    const input = body(context, ['peerDeviceId']);
    const peerDeviceId = validate.opaqueId(input, 'peerDeviceId');
    const peer = await store.getDevice(peerDeviceId);
    if (!peer || peer.accountId !== caller.accountId) {
      throw new HttpError(404, 'not_found', 'no such device');
    }
    const pairing = await pairingBetween(caller, peer);
    if (!pairing || peer.revokedAt !== null) {
      await audit(caller.accountId, peer.id, 'turn.credentials', 'denied');
      throw forbidden('TURN credentials require an active pairing');
    }
    const account = await store.getAccount(caller.accountId);
    if (!account?.relayEnabled) {
      await audit(caller.accountId, peer.id, 'turn.credentials', 'denied');
      throw forbidden('relay access is disabled for this account');
    }
    if (!turn) {
      throw new HttpError(503, 'relay_not_configured', 'Cloudflare TURN is not configured');
    }
    const nowSeconds = unixSeconds(now);
    let iceServers;
    try {
      iceServers = await turn.issue(config.turnCredentialTtlSeconds);
    } catch {
      await audit(caller.accountId, peer.id, 'turn.credentials', 'denied');
      throw new HttpError(503, 'relay_unavailable', 'TURN credential service is unavailable');
    }
    const usernames = iceServers.flatMap((server) => server.username ? [server.username] : []);
    await Promise.all(usernames.map((username) => store.createTurnCredential({
      accountId: caller.accountId, deviceId: caller.id, username,
      expiresAt: nowSeconds + config.turnCredentialTtlSeconds,
    })));
    await audit(caller.accountId, peer.id, 'turn.credentials', 'allowed');
    return {
      status: 201,
      body: {
        iceServers,
        expiresAt: nowSeconds + config.turnCredentialTtlSeconds,
      },
    };
  });

  return router;
}

/** Maps validation failures onto the HTTP error contract. */
export function withValidationMapping(handler: Handler): Handler {
  return async (context) => {
    try {
      return await handler(context);
    } catch (error) {
      if (error instanceof ValidationError) {
        throw new HttpError(400, 'invalid_request', error.message, error.field);
      }
      throw error;
    }
  };
}
