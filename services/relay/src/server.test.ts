import assert from 'node:assert/strict';
import { generateKeyPairSync, sign } from 'node:crypto';
import { test } from 'node:test';
import WebSocket from 'ws';

import type { AdmissionClaim, RelayRole } from './claims.ts';
import { createRelayServer } from './server.ts';

const issuer = 'https://control.test';
const keys = generateKeyPairSync('ed25519');
const publicPem = keys.publicKey.export({ type: 'spki', format: 'pem' }).toString();

function token(role: RelayRole, overrides: Partial<AdmissionClaim> = {}): string {
  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(JSON.stringify({ alg: 'EdDSA', kid: 'test-key' })).toString('base64url');
  const claim: AdmissionClaim = {
    iss: issuer,
    aud: 'latch-relay',
    kid: 'test-key',
    roomId: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    role,
    purpose: 'session',
    jti: `ticket_${role}_00000001`,
    generation: 1,
    nbf: now - 1,
    exp: now + 60,
    limits: { maxStreams: 32, streamWindowBytes: 262144, totalBufferBytes: 8388608, recordBytes: 65535 },
    ...overrides,
  };
  const payload = Buffer.from(JSON.stringify(claim)).toString('base64url');
  const signature = sign(null, Buffer.from(`${header}.${payload}`), keys.privateKey).toString('base64url');
  return `${header}.${payload}.${signature}`;
}

function extension(role: RelayRole, leaseId: string): string {
  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(JSON.stringify({ alg: 'EdDSA', kid: 'test-key' })).toString('base64url');
  const claim = {
    iss: issuer, aud: 'latch-relay', kid: 'test-key',
    roomId: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', role, purpose: 'session',
    jti: `extension_${role}_0001`, generation: 1, nbf: now - 1, exp: now + 600,
    limits: { maxStreams: 32, streamWindowBytes: 262144, totalBufferBytes: 8388608, recordBytes: 65535 },
    leaseId,
  };
  const payload = Buffer.from(JSON.stringify(claim)).toString('base64url');
  const signature = sign(null, Buffer.from(`${header}.${payload}`), keys.privateKey).toString('base64url');
  return `${header}.${payload}.${signature}`;
}

function open(url: string, admission: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, { headers: { authorization: `Bearer ${admission}` } });
    socket.once('open', () => resolve(socket));
    socket.once('error', reject);
  });
}

function binary(socket: WebSocket): Promise<Buffer> {
  return new Promise((resolve) => {
    socket.on('message', (data, isBinary) => { if (isBinary) resolve(Buffer.from(data as Buffer)); });
  });
}

test('forwards only opaque binary records after both roles redeem', async () => {
  const redeemed: string[] = [];
  const relay = createRelayServer({
    host: '127.0.0.1', port: 0, issuer,
    publicKeys: new Map([['test-key', publicPem]]),
    invalidationSecret: 'invalidation-secret-32-bytes-long',
    redeem: async (claim) => {
      redeemed.push(claim.jti);
      return { leaseId: `lease-${claim.role}`, expiresAt: Math.floor(Date.now() / 1000) + 600 };
    },
  });
  const port = await relay.listen();
  const host = await open(`ws://127.0.0.1:${port}/v1/connect`, token('host'));
  const controller = await open(`ws://127.0.0.1:${port}/v1/connect`, token('controller'));
  const received = binary(host);
  controller.send(Buffer.from('noise-ciphertext'));
  assert.equal((await received).toString(), 'noise-ciphertext');
  assert.equal(redeemed.length, 2);
  await relay.drain();
});

test('rejects wrong audience and does not call redemption', async () => {
  let redeemed = false;
  const relay = createRelayServer({
    host: '127.0.0.1', port: 0, issuer,
    publicKeys: new Map([['test-key', publicPem]]),
    invalidationSecret: 'invalidation-secret-32-bytes-long',
    redeem: async () => { redeemed = true; return { leaseId: 'bad', expiresAt: 0 }; },
  });
  const port = await relay.listen();
  await assert.rejects(open(`ws://127.0.0.1:${port}/v1/connect`, token('host', { aud: 'wrong' as 'latch-relay' })));
  assert.equal(redeemed, false);
  await relay.drain();
});

test('authenticated invalidation closes both room roles', async () => {
  const secret = 'invalidation-secret-32-bytes-long';
  const relay = createRelayServer({
    host: '127.0.0.1', port: 0, issuer,
    publicKeys: new Map([['test-key', publicPem]]), invalidationSecret: secret,
    redeem: async (claim) => ({ leaseId: `lease-${claim.role}`, expiresAt: Math.floor(Date.now() / 1000) + 600 }),
  });
  const port = await relay.listen();
  const host = await open(`ws://127.0.0.1:${port}/v1/connect`, token('host'));
  const controller = await open(`ws://127.0.0.1:${port}/v1/connect`, token('controller'));
  const closed = Promise.all([
    new Promise<void>((resolve) => host.once('close', () => resolve())),
    new Promise<void>((resolve) => controller.once('close', () => resolve())),
  ]);
  const response = await fetch(`http://127.0.0.1:${port}/private/v1/invalidate`, {
    method: 'POST', headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
    body: JSON.stringify({ roomId: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', notAfter: Math.floor(Date.now() / 1000) + 600 }),
  });
  assert.equal(response.status, 204);
  await closed;
  await relay.drain();
});

test('a newer role generation replaces the room without blocking admission', async () => {
  const relay = createRelayServer({
    host: '127.0.0.1', port: 0, issuer,
    publicKeys: new Map([['test-key', publicPem]]),
    invalidationSecret: 'invalidation-secret-32-bytes-long',
    redeem: async (claim) => ({
      leaseId: `lease-${claim.role}-${claim.generation}`,
      expiresAt: Math.floor(Date.now() / 1000) + 600,
    }),
  });
  const port = await relay.listen();
  const firstHost = await open(`ws://127.0.0.1:${port}/v1/connect`, token('host'));
  const firstController = await open(`ws://127.0.0.1:${port}/v1/connect`, token('controller'));
  const oldRoomClosed = Promise.all([
    new Promise<void>((resolve) => firstHost.once('close', () => resolve())),
    new Promise<void>((resolve) => firstController.once('close', () => resolve())),
  ]);
  const host = await open(`ws://127.0.0.1:${port}/v1/connect`, token('host', {
    generation: 2, jti: 'ticket_host_00000002',
  }));
  await oldRoomClosed;
  const controller = await open(`ws://127.0.0.1:${port}/v1/connect`, token('controller', {
    generation: 2, jti: 'ticket_controller_00000002',
  }));
  const received = binary(host);
  controller.send(Buffer.from('replacement-room'));
  assert.equal((await received).toString(), 'replacement-room');
  await relay.drain();
});

test('accepts only a matching signed lease extension on the WSS control channel', async () => {
  const relay = createRelayServer({
    host: '127.0.0.1', port: 0, issuer,
    publicKeys: new Map([['test-key', publicPem]]),
    invalidationSecret: 'invalidation-secret-32-bytes-long',
    redeem: async (claim) => ({ leaseId: `lease-${claim.role}-00000001`, expiresAt: Math.floor(Date.now() / 1000) + 1 }),
  });
  const port = await relay.listen();
  const host = await open(`ws://127.0.0.1:${port}/v1/connect`, token('host'));
  const controller = await open(`ws://127.0.0.1:${port}/v1/connect`, token('controller'));
  host.send(JSON.stringify({ type: 'lease_extension', claim: extension('host', 'lease-host-00000001') }));
  controller.send(JSON.stringify({ type: 'lease_extension', claim: extension('controller', 'lease-controller-00000001') }));
  await new Promise((resolve) => setTimeout(resolve, 1_100));
  assert.equal(controller.readyState, WebSocket.OPEN);
  host.close();
  await relay.drain();
});
