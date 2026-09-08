/**
 * The Latch control-plane HTTP API.
 *
 * Responsibilities, and their limits:
 *
 * - account and device registration, and device revocation;
 * - a paired-device directory that mirrors grants a host device approved
 *   locally, never grants the control plane invents;
 * - short-lived, single-use Remote Link admission and renewable leases;
 * - durable relay-room invalidation after revoke.
 *
 * What never passes through here: terminal bytes, transcripts, session names,
 * prompt answers, device private keys, and the Latch gateway bearer token.
 * It coordinates opaque room admission only; both endpoints authenticate the
 * exact peer key through the shared Remote Link protocol.
 */

import type { Config } from './config.ts';
import type { Device, Permission } from './domain.ts';
import { randomBytes } from 'node:crypto';

import { REMOTE_LINK_LIMITS, signAdmission } from './admission.ts';
import type { ApnsSender } from './apns.ts';
import { bearerToken, digestOf, digestsMatch, issueCredential, newId, newSecret, subjectOf } from './credentials.ts';
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
  /** Attention delivery, when configured. */
  readonly apns?: ApnsSender | null;
}

const unauthorized = (): HttpError =>
  new HttpError(401, 'unauthorized', 'a valid bearer credential is required');

const forbidden = (message: string): HttpError => new HttpError(403, 'forbidden', message);

const OWNER_INVITATION_TTL_SECONDS = 10 * 60;
const ENROLLMENT_TTL_SECONDS = 5 * 60;
const ADMISSION_TTL_SECONDS = 60;
const LEASE_TTL_SECONDS = 10 * 60;
/** An undelivered attention alert is stale after this; the phone refreshes anyway. */
const ATTENTION_TTL_SECONDS = 10 * 60;
const PUSH_TOKEN = /^[0-9a-f]{64}$/;
const EVENT_ID = /^[0-9a-f]{32}$/;

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
  const { config, store, now } = dependencies;
  const limiter = new RateLimiter(config.rateLimitPerMinute, 60_000, now);
  const admissionPerDevice = new RateLimiter(config.admissionRatePerDevice, 60_000, now);
  const admissionPerOwner = new RateLimiter(config.admissionRatePerOwner, 60_000, now);
  const admissionPerIp = new RateLimiter(config.admissionRatePerIp, 60_000, now);
  const admissionGlobal = new RateLimiter(config.admissionRateGlobal, 60_000, now);
  const attentionPerHost = new RateLimiter(config.attentionRatePerHost, 60_000, now);

  function requireAdmissionBudget(context: RequestContext, accountId: string, deviceId?: string): void {
    const allowed = admissionGlobal.allow('deployment') &&
      admissionPerOwner.allow(accountId) &&
      admissionPerIp.allow(context.sourceIp) &&
      (!deviceId || admissionPerDevice.allow(deviceId));
    if (!allowed) throw new HttpError(429, 'remote_admission_limited', 'remote-link admission budget exhausted');
  }

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

  const body = (context: RequestContext, allowed: readonly string[]) =>
    validate.object(context.body ?? {}, allowed);

  function requireOperator(context: RequestContext): void {
    const token = bearerToken(context.headers.authorization);
    if (!token || !config.operatorSecret || !digestsMatch(token, config.operatorSecret)) throw unauthorized();
  }

  function requireRelayService(context: RequestContext): void {
    const token = bearerToken(context.headers.authorization);
    if (!token || !config.relayServiceToken || !digestsMatch(token, config.relayServiceToken)) throw unauthorized();
  }

  async function issueRemoteAdmission(input: {
    linkId: string | null;
    enrollmentId: string | null;
    roomId: string;
    role: 'host' | 'controller';
    purpose: 'enrollment' | 'session';
    generation: number;
  }) {
    if (!config.admissionPrivateKeyPem) throw new HttpError(503, 'relay_not_configured', 'relay admission signing is not configured');
    const at = unixSeconds(now);
    const id = newId('adm');
    await store.createRemoteAdmission({ ...input, id, expiresAt: at + ADMISSION_TTL_SECONDS });
    const claim = signAdmission({
      iss: config.admissionIssuer, aud: 'latch-relay', kid: config.admissionKeyId,
      roomId: input.roomId, role: input.role, purpose: input.purpose, jti: id,
      generation: input.generation, nbf: at, exp: at + ADMISSION_TTL_SECONDS,
      limits: REMOTE_LINK_LIMITS,
    }, config.admissionPrivateKeyPem);
    return { claim, expiresAt: at + ADMISSION_TTL_SECONDS };
  }

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
          relayConfigured: Boolean(config.admissionPrivateKeyPem && config.relayServiceToken),
          apnsConfigured: Boolean(dependencies.apns),
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

  router.post('/v1/operator/owner-invitations', async (context) => {
    requireOperator(context);
    const input = body(context, ['ttlSeconds']);
    const ttl = input.ttlSeconds === undefined ? OWNER_INVITATION_TTL_SECONDS : input.ttlSeconds;
    if (!Number.isSafeInteger(ttl) || Number(ttl) < 60 || Number(ttl) > OWNER_INVITATION_TTL_SECONDS) {
      throw new ValidationError('ttlSeconds', 'ttlSeconds must be an integer between 60 and 600');
    }
    const id = newId('inv');
    const credential = issueCredential('owner-invitation', id);
    await store.createOwnerInvitation({ id, secretDigest: credential.digest, expiresAt: unixSeconds(now) + Number(ttl) });
    return { status: 201, body: { invitation: credential.token, expiresAt: unixSeconds(now) + Number(ttl) } };
  });

  router.post('/v1/accounts/claim', async (context) => {
    const input = body(context, ['invitation', 'label']);
    const invitation = validate.requiredString(input, 'invitation', /^inv_[0-9a-f]{32}\.[0-9a-f]{64}$/);
    const invitationId = subjectOf(invitation)!;
    const consumed = await store.consumeOwnerInvitation(
      invitationId, digestOf('owner-invitation', invitation), new Date(now()).toISOString(), unixSeconds(now),
    );
    if (!consumed) throw forbidden('owner invitation is invalid, expired, or already used');
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
      entries.push({
        ...deviceView(device, permissions.get(device.id) ?? null),
        self: device.id === caller.id,
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

  /** Revocation immediately invalidates current Remote Link grants and rooms. */
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
    await audit(caller.accountId, peer.id, 'pairing.revoke', 'allowed');
    return { status: 200, body: { hostDeviceId: host, clientDeviceId: client, revoked: true } };
  });

  // --- Remote Link v1 enrollment and opaque relay admission ----------------

  router.post('/v1/enrollments', async (context) => {
    const host = await requireDevice(context);
    body(context, []);
    if (host.role !== 'host') throw forbidden('only a host device may open enrollment');
    const account = await store.getAccount(host.accountId);
    if (!account?.relayEnabled) throw forbidden('remote access is disabled for this account');
    requireAdmissionBudget(context, host.accountId, host.id);
    const id = newId('enr');
    const roomId = randomBytes(32).toString('base64url');
    const admissionCode = issueCredential('enrollment', id);
    const expiresAt = unixSeconds(now) + ENROLLMENT_TTL_SECONDS;
    await store.createRemoteEnrollment({
      id, accountId: host.accountId, hostDeviceId: host.id, roomId,
      admissionDigest: admissionCode.digest, expiresAt,
    });
    const hostAdmission = await issueRemoteAdmission({
      linkId: null, enrollmentId: id, roomId, role: 'host', purpose: 'enrollment', generation: 1,
    });
    await audit(host.accountId, host.id, 'enrollment.open', 'allowed');
    return {
      status: 201,
      body: {
        version: 1, enrollmentId: id, expiresAt, relayUrl: config.relayUrl,
        hostPublicKey: host.publicKey, admissionCode: admissionCode.token,
        hostAdmission: hostAdmission.claim,
      },
    };
  });

  router.post('/v1/enrollments/:enrollmentId/claim', async (context) => {
    const enrollmentId = context.params.enrollmentId ?? '';
    if (!validate.isOpaqueId(enrollmentId)) throw new HttpError(404, 'not_found', 'no such enrollment');
    const input = body(context, ['admissionCode', 'name', 'platform', 'publicKey']);
    const admissionCode = validate.requiredString(input, 'admissionCode', /^enr_[0-9a-f]{32}\.[0-9a-f]{64}$/);
    if (subjectOf(admissionCode) !== enrollmentId) throw forbidden('enrollment admission does not match');
    const provisionalDeviceId = newId('dev');
    const provisionalCredential = issueCredential('device', provisionalDeviceId);
    const claimed = await store.claimRemoteEnrollment({
      id: enrollmentId,
      admissionDigest: digestOf('enrollment', admissionCode),
      provisionalDeviceId,
      provisionalName: validate.requiredLabel(input, 'name'),
      provisionalPlatform: validate.requiredString(input, 'platform', /^[a-z0-9.-]{2,32}$/),
      provisionalPublicKey: validate.publicKey(input, 'publicKey'),
      provisionalTokenDigest: provisionalCredential.digest,
      now: unixSeconds(now),
    });
    if (!claimed) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is invalid, expired, or already claimed');
    const enrollment = await store.getRemoteEnrollment(enrollmentId, unixSeconds(now));
    if (!enrollment) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is unavailable');
    requireAdmissionBudget(context, enrollment.accountId, provisionalDeviceId);
    const controllerAdmission = await issueRemoteAdmission({
      linkId: null, enrollmentId, roomId: enrollment.roomId,
      role: 'controller', purpose: 'enrollment', generation: 1,
    });
    await audit(enrollment.accountId, provisionalDeviceId, 'enrollment.claim', 'allowed');
    return {
      status: 201,
      body: {
        version: 1, enrollmentId, provisionalDeviceId,
        provisionalToken: provisionalCredential.token,
        relayUrl: config.relayUrl, controllerAdmission: controllerAdmission.claim,
      },
    };
  });

  router.post('/v1/enrollments/:enrollmentId/complete', async (context) => {
    const host = await requireDevice(context);
    if (host.role !== 'host') throw forbidden('only a host device may complete enrollment');
    const enrollmentId = context.params.enrollmentId ?? '';
    const enrollment = await store.getRemoteEnrollment(enrollmentId, unixSeconds(now));
    if (!enrollment || enrollment.hostDeviceId !== host.id || enrollment.accountId !== host.accountId) {
      throw new HttpError(404, 'not_found', 'no such enrollment');
    }
    const input = body(context, ['controllerPublicKey', 'permission', 'grantRevision']);
    const controllerPublicKey = validate.publicKey(input, 'controllerPublicKey');
    const permission = validate.permission(input, 'permission', 'interact');
    const grantRevision = input.grantRevision;
    if (!Number.isSafeInteger(grantRevision) || Number(grantRevision) !== 1) {
      throw new ValidationError('grantRevision', 'initial grantRevision must be 1');
    }
    if (!enrollment.provisionalDeviceId || !enrollment.provisionalTokenDigest ||
        enrollment.provisionalPublicKey !== controllerPublicKey || !enrollment.provisionalName || !enrollment.provisionalPlatform) {
      await audit(host.accountId, enrollment.provisionalDeviceId, 'enrollment.complete', 'denied');
      throw forbidden('approval does not match the encrypted enrollment proposal');
    }
    const link = await store.finalizeRemoteEnrollment({
      id: enrollmentId, hostDeviceId: host.id, controllerPublicKey, permission,
      linkId: newId('link'), linkRoomId: randomBytes(32).toString('base64url'),
      completedAt: new Date(now()).toISOString(), now: unixSeconds(now),
      maxDevices: config.maxDevicesPerAccount,
    });
    if (!link) throw new HttpError(409, 'enrollment_unavailable', 'enrollment was already completed, cancelled, or over entitlement');
    await audit(host.accountId, enrollment.provisionalDeviceId, 'enrollment.complete', 'allowed');
    return {
      status: 200,
      body: {
        version: 1, enrollmentId, hostPublicKey: host.publicKey,
        controllerPublicKey, permission, grantRevision: link.grantRevision,
        remoteLinkId: link.id,
      },
    };
  });

  router.delete('/v1/enrollments/:enrollmentId', async (context) => {
    const host = await requireDevice(context);
    const enrollment = await store.getRemoteEnrollment(context.params.enrollmentId ?? '', unixSeconds(now));
    if (!enrollment || enrollment.hostDeviceId !== host.id) throw new HttpError(404, 'not_found', 'no such enrollment');
    if (!(await store.cancelRemoteEnrollment(enrollment.id, new Date(now()).toISOString()))) {
      throw new HttpError(409, 'enrollment_unavailable', 'enrollment cannot be cancelled');
    }
    return { status: 200, body: { enrollmentId: enrollment.id, cancelled: true } };
  });

  router.get('/v1/remote-links', async (context) => {
    const caller = await requireDevice(context);
    const pairings = await store.listPairingsForDevice(caller.id);
    const links = [];
    for (const pairing of pairings) {
      const link = await store.getOrCreateRemoteLink(
        pairing.accountId, pairing.hostDeviceId, pairing.clientDeviceId, randomBytes(32).toString('base64url'),
      );
      const peerId = caller.id === pairing.hostDeviceId ? pairing.clientDeviceId : pairing.hostDeviceId;
      const peer = await store.getDevice(peerId);
      if (peer && peer.revokedAt === null) {
        links.push({
          version: 1, linkId: link.id, peerDeviceId: peer.id, peerPublicKey: peer.publicKey,
          permission: pairing.permission, grantRevision: link.grantRevision,
        });
      }
    }
    return { status: 200, body: { links } };
  });

  router.post('/v1/relay-admissions', async (context) => {
    const caller = await requireDevice(context);
    const input = body(context, ['peerDeviceId']);
    const peer = await store.getDevice(validate.opaqueId(input, 'peerDeviceId'));
    if (!peer || peer.accountId !== caller.accountId) throw new HttpError(404, 'not_found', 'no such device');
    const pairing = await pairingBetween(caller, peer);
    const account = await store.getAccount(caller.accountId);
    if (!pairing || peer.revokedAt !== null || !account?.relayEnabled) {
      await audit(caller.accountId, peer.id, 'relay.admission', 'denied');
      throw forbidden('relay admission requires an active entitled pairing');
    }
    requireAdmissionBudget(context, caller.accountId, caller.id);
    const hostId = caller.role === 'host' ? caller.id : peer.id;
    const clientId = caller.role === 'client' ? caller.id : peer.id;
    const link = await store.getOrCreateRemoteLink(
      caller.accountId, hostId, clientId, randomBytes(32).toString('base64url'),
    );
    const role = caller.role === 'host' ? 'host' : 'controller';
    const generation = role === 'host' ? link.hostGeneration + 1 : link.controllerGeneration + 1;
    const admission = await issueRemoteAdmission({
      linkId: link.id, enrollmentId: null, roomId: link.roomId,
      role, purpose: 'session', generation,
    });
    await audit(caller.accountId, peer.id, 'relay.admission', 'allowed');
    return { status: 201, body: { version: 1, relayUrl: config.relayUrl, admission: admission.claim, expiresAt: admission.expiresAt } };
  });

  // --- Attention notifications --------------------------------------------

  /** A controller registers the APNs token the app received. One per device. */
  router.put('/v1/push-registrations', async (context) => {
    const caller = await requireDevice(context);
    const input = body(context, ['pushToken']);
    if (caller.role !== 'client') throw forbidden('only a controller device receives notifications');
    const pushToken = validate.requiredString(input, 'pushToken', PUSH_TOKEN);
    await store.upsertPushRegistration(caller.id, pushToken, new Date(now()).toISOString());
    await audit(caller.accountId, caller.id, 'push.register', 'allowed');
    return { status: 200, body: { registered: true } };
  });

  router.delete('/v1/push-registrations', async (context) => {
    const caller = await requireDevice(context);
    await store.deletePushRegistration(caller.id);
    await audit(caller.accountId, caller.id, 'push.unregister', 'allowed');
    return { status: 200, body: { registered: false } };
  });

  /**
   * A host asks for one generic attention alert to a paired phone. The body
   * names the phone and an opaque event id; nothing about the session, the
   * prompt, or the approval travels here, and the payload sent to APNs is a
   * fixed sentence. Delivery is best effort and never required for the
   * phone's own foreground refresh.
   */
  router.post('/v1/attention', async (context) => {
    const host = await requireDevice(context);
    const input = body(context, ['clientDeviceId', 'eventId']);
    if (host.role !== 'host') throw forbidden('only a host device may request attention');
    const clientDeviceId = validate.opaqueId(input, 'clientDeviceId');
    const eventId = validate.requiredString(input, 'eventId', EVENT_ID);
    const client = await store.getDevice(clientDeviceId);
    const pairing = client ? await store.getPairing(host.id, client.id) : null;
    if (!client || client.accountId !== host.accountId || client.revokedAt !== null || !pairing) {
      await audit(host.accountId, clientDeviceId, 'attention.notify', 'denied');
      throw forbidden('attention requires an active pairing with that device');
    }
    if (!attentionPerHost.allow(host.id)) {
      throw new HttpError(429, 'attention_limited', 'attention notification budget exhausted');
    }
    const fresh = await store.recordAttentionEvent(host.id, client.id, eventId, new Date(now()).toISOString());
    if (!fresh) {
      return { status: 202, body: { eventId, delivered: false, reason: 'duplicate' } };
    }
    const registration = await store.getPushRegistration(client.id);
    if (!registration) {
      return { status: 202, body: { eventId, delivered: false, reason: 'unregistered' } };
    }
    if (!dependencies.apns) {
      return { status: 202, body: { eventId, delivered: false, reason: 'unconfigured' } };
    }
    const outcome = await dependencies.apns.send({
      token: registration.pushToken,
      collapseId: eventId,
      expiresAt: unixSeconds(now) + ATTENTION_TTL_SECONDS,
    });
    if (outcome === 'invalid_token') {
      // Apple says this token will never work again; keeping it would only
      // repeat the failure on every event.
      await store.deletePushRegistration(client.id);
    }
    await audit(host.accountId, client.id, 'attention.notify', outcome === 'delivered' ? 'allowed' : 'denied');
    return {
      status: 202,
      body: { eventId, delivered: outcome === 'delivered', ...(outcome === 'delivered' ? {} : { reason: outcome }) },
    };
  });

  router.post('/private/v1/relay/redemptions', async (context) => {
    requireRelayService(context);
    const input = body(context, ['ticketId', 'attemptId']);
    const ticketId = validate.opaqueId(input, 'ticketId');
    const attemptId = validate.requiredString(input, 'attemptId', /^[A-Za-z0-9_-]{16,96}$/);
    const admission = await store.getRemoteAdmission(ticketId);
    if (!admission) throw forbidden('admission is unavailable');
    if (admission.linkId) {
      const link = await store.getRemoteLink(admission.linkId);
      if (!link) throw forbidden('link is unavailable');
      const account = await store.getAccount(link.accountId);
      const host = await store.getDevice(link.hostDeviceId);
      const controller = await store.getDevice(link.clientDeviceId);
      const pairing = await store.getPairing(link.hostDeviceId, link.clientDeviceId);
      const currentGeneration = admission.role === 'host' ? link.hostGeneration : link.controllerGeneration;
      if (!account?.relayEnabled || !host || host.revokedAt || !controller || controller.revokedAt || !pairing ||
          admission.generation !== currentGeneration) {
        throw forbidden('admission authorization is no longer current');
      }
    } else if (admission.enrollmentId) {
      const enrollment = await store.getRemoteEnrollment(admission.enrollmentId, unixSeconds(now));
      if (!enrollment || (admission.role === 'controller' && !enrollment.provisionalDeviceId)) {
        throw forbidden('enrollment authorization is no longer current');
      }
    }
    const leaseId = newId('lease');
    const redeemed = await store.redeemRemoteAdmission(
      ticketId, attemptId, leaseId, unixSeconds(now) + LEASE_TTL_SECONDS, unixSeconds(now),
    );
    if (!redeemed?.leaseId || !redeemed.leaseExpiresAt) throw forbidden('admission was already spent');
    return { status: 200, body: { leaseId: redeemed.leaseId, expiresAt: redeemed.leaseExpiresAt } };
  });

  router.post('/v1/relay-leases/:leaseId/renew', async (context) => {
    const caller = await requireDevice(context);
    const leaseId = context.params.leaseId ?? '';
    if (!/^lease_[0-9a-f]{32}$/.test(leaseId)) throw new HttpError(404, 'not_found', 'no such lease');
    const current = await store.getRemoteAdmissionByLease(leaseId);
    if (!current?.linkId || !current.leaseId || !config.admissionPrivateKeyPem) throw forbidden('lease is unavailable');
    const link = await store.getRemoteLink(current.linkId);
    if (!link) throw forbidden('link is unavailable');
    const expectedDevice = current.role === 'host' ? link.hostDeviceId : link.clientDeviceId;
    const pairing = await store.getPairing(link.hostDeviceId, link.clientDeviceId);
    const account = await store.getAccount(link.accountId);
    const currentGeneration = current.role === 'host' ? link.hostGeneration : link.controllerGeneration;
    if (caller.id !== expectedDevice || !pairing || !account?.relayEnabled || current.generation !== currentGeneration) {
      throw forbidden('lease authorization is no longer current');
    }
    const at = unixSeconds(now);
    const expiresAt = at + LEASE_TTL_SECONDS;
    const extended = await store.extendRemoteLease(leaseId, expiresAt, at);
    if (!extended) throw forbidden('lease is unavailable');
    const claim = signAdmission({
      iss: config.admissionIssuer, aud: 'latch-relay', kid: config.admissionKeyId,
      roomId: current.roomId, role: current.role, purpose: current.purpose,
      jti: newId('ext'), generation: current.generation, nbf: at, exp: expiresAt,
      limits: REMOTE_LINK_LIMITS, leaseId: current.leaseId,
    }, config.admissionPrivateKeyPem);
    return { status: 200, body: { leaseId, expiresAt, extension: claim } };
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
