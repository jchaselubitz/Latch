import { createServer as createHttpServer, type IncomingMessage, type Server as HttpServer } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { readFileSync } from 'node:fs';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { WebSocketServer, WebSocket, type RawData } from 'ws';

import { verifyAdmissionClaim, verifyLeaseExtensionClaim, type AdmissionClaim, type RelayRole } from './claims.ts';

const MAX_RECORD_BYTES = 65_535;
const MAX_BUFFERED_BYTES = 8 * 1024 * 1024;
const HEARTBEAT_MS = 15_000;
const DEFAULT_MAX_CONNECTIONS = 512;
const DEFAULT_MAX_CONNECTIONS_PER_IP = 16;
const DEFAULT_BYTES_PER_LEASE = 256 * 1024 * 1024;

export interface Lease {
  readonly leaseId: string;
  readonly expiresAt: number;
}

export interface RelayOptions {
  readonly host: string;
  readonly port: number;
  readonly issuer: string;
  readonly publicKeys: ReadonlyMap<string, string>;
  readonly invalidationSecret: string;
  readonly redeem: (claim: AdmissionClaim, attemptId: string) => Promise<Lease>;
  readonly now?: () => number;
  readonly tls?: { readonly certPath: string; readonly keyPath: string };
  readonly maxConnections?: number;
  readonly maxConnectionsPerIp?: number;
  readonly bytesPerLease?: number;
}

interface Peer {
  readonly socket: WebSocket;
  readonly claim: AdmissionClaim;
  lease: Lease;
  readonly attemptId: string;
  leaseTimer: NodeJS.Timeout;
  alive: boolean;
  bytesForwarded: number;
  readonly sourceIp: string;
}

interface Room {
  host?: Peer;
  controller?: Peer;
}

function secretMatches(expected: string, header: string | undefined): boolean {
  const supplied = header?.replace(/^Bearer\s+/i, '') ?? '';
  const left = Buffer.from(expected);
  const right = Buffer.from(supplied);
  return left.length === right.length && timingSafeEqual(left, right);
}

async function body(request: IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  let length = 0;
  for await (const chunk of request) {
    length += (chunk as Buffer).length;
    if (length > 4096) throw new Error('body too large');
    chunks.push(chunk as Buffer);
  }
  const value = JSON.parse(Buffer.concat(chunks).toString('utf8')) as unknown;
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid body');
  return value as Record<string, unknown>;
}

/** Creates the independently deployable opaque relay. */
export function createRelayServer(options: RelayOptions): {
  readonly server: HttpServer;
  readonly listen: () => Promise<number>;
  readonly drain: () => Promise<void>;
} {
  const now = options.now ?? (() => Date.now());
  const rooms = new Map<string, Room>();
  const connectionsByIp = new Map<string, number>();
  let accepting = true;
  const listener = async (request: IncomingMessage, response: import('node:http').ServerResponse) => {
    if (request.method === 'GET' && request.url === '/health/live') {
      response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
      response.end('{"live":true}');
      return;
    }
    if (request.method === 'GET' && request.url === '/health/ready') {
      response.writeHead(accepting ? 200 : 503, { 'content-type': 'application/json', 'cache-control': 'no-store' });
      response.end(JSON.stringify({ ready: accepting }));
      return;
    }
    if (request.method === 'POST' && request.url === '/private/v1/invalidate') {
      if (!secretMatches(options.invalidationSecret, request.headers.authorization)) {
        response.writeHead(401).end();
        return;
      }
      try {
        const value = await body(request);
        const roomId = String(value.roomId ?? '');
        if (Object.keys(value).sort().join(',') !== 'notAfter,roomId' ||
            !/^[A-Za-z0-9_-]{43}$/.test(roomId) || !Number.isSafeInteger(value.notAfter)) throw new Error();
        closeRoom(roomId, 4003, 'revoked');
        response.writeHead(204).end();
      } catch {
        response.writeHead(400).end();
      }
      return;
    }
    response.writeHead(404).end();
  };
  const server = options.tls
    ? createHttpsServer({ cert: readFileSync(options.tls.certPath), key: readFileSync(options.tls.keyPath) }, listener)
    : createHttpServer(listener);
  const websocket = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: MAX_RECORD_BYTES });

  function peer(room: Room, role: RelayRole): Peer | undefined {
    return role === 'host' ? room.host : room.controller;
  }

  function setPeer(room: Room, role: RelayRole, value: Peer | undefined): void {
    if (role === 'host') room.host = value;
    else room.controller = value;
  }

  function closePeer(value: Peer | undefined, code: number, reason: string): void {
    if (!value) return;
    clearTimeout(value.leaseTimer);
    if (value.socket.readyState === WebSocket.OPEN || value.socket.readyState === WebSocket.CONNECTING) {
      value.socket.close(code, reason);
    }
  }

  function closeRoom(roomId: string, code: number, reason: string): void {
    const room = rooms.get(roomId);
    if (!room) return;
    rooms.delete(roomId);
    closePeer(room.host, code, reason);
    closePeer(room.controller, code, reason);
  }

  server.on('upgrade', async (request, socket, head) => {
    if (!accepting || request.url !== '/v1/connect') {
      socket.write('HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    const sourceIp = (socket as import('node:net').Socket).remoteAddress ?? 'unknown';
    const active = [...connectionsByIp.values()].reduce((sum, count) => sum + count, 0);
    if (active >= (options.maxConnections ?? DEFAULT_MAX_CONNECTIONS) ||
        (connectionsByIp.get(sourceIp) ?? 0) >= (options.maxConnectionsPerIp ?? DEFAULT_MAX_CONNECTIONS_PER_IP)) {
      socket.write('HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    const match = /^Bearer\s+([A-Za-z0-9._-]+)$/.exec(request.headers.authorization ?? '');
    if (!match) {
      socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
    } catch {
      socket.write('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
      socket.destroy();
    }
  });

  function admit(socket: WebSocket, claim: AdmissionClaim, lease: Lease, attemptId: string, sourceIp: string): void {
    let room = rooms.get(claim.roomId);
    if (!room) {
      room = {};
      rooms.set(claim.roomId, room);
    }
    const existing = peer(room, claim.role);
    if (existing && existing.claim.generation >= claim.generation) {
      socket.close(4004, 'stale generation');
      return;
    }
    if (existing) closeRoom(claim.roomId, 4001, 'role replaced');
    room = rooms.get(claim.roomId) ?? {};
    rooms.set(claim.roomId, room);
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
    socket.on('close', () => {
      const remaining = (connectionsByIp.get(sourceIp) ?? 1) - 1;
      if (remaining <= 0) connectionsByIp.delete(sourceIp); else connectionsByIp.set(sourceIp, remaining);
      const current = rooms.get(claim.roomId);
      if (!current || peer(current, claim.role) !== value) return;
      closeRoom(claim.roomId, 1000, 'peer gone');
    });
    const other = peer(room, claim.role === 'host' ? 'controller' : 'host');
    if (other && other.claim.purpose === claim.purpose) {
      socket.send(JSON.stringify({ type: 'peer_ready' }));
      other.socket.send(JSON.stringify({ type: 'peer_ready' }));
    }
  }

  function forward(source: Peer, data: RawData, isBinary: boolean): void {
    const room = rooms.get(source.claim.roomId);
    if (!room || peer(room, source.claim.role) !== source) return;
    if (!isBinary) {
      try {
        const control = JSON.parse(data.toString()) as unknown;
        if (!control || typeof control !== 'object' || Array.isArray(control)) throw new Error();
        const entry = control as Record<string, unknown>;
        if (Object.keys(entry).sort().join(',') !== 'claim,type' || entry.type !== 'lease_extension' || typeof entry.claim !== 'string') throw new Error();
        const extension = verifyLeaseExtensionClaim(entry.claim, options.publicKeys, options.issuer, Math.floor(now() / 1000));
        if (extension.leaseId !== source.lease.leaseId || extension.roomId !== source.claim.roomId ||
            extension.role !== source.claim.role || extension.purpose !== source.claim.purpose ||
            extension.generation !== source.claim.generation) throw new Error();
        clearTimeout(source.leaseTimer);
        source.lease = { leaseId: extension.leaseId, expiresAt: extension.exp };
        source.leaseTimer = setTimeout(
          () => closeRoom(source.claim.roomId, 4002, 'lease expired'),
          Math.max(0, extension.exp * 1000 - now()),
        );
      } catch {
        closeRoom(source.claim.roomId, 4005, 'invalid control message');
      }
      return;
    }
    const bytes = Buffer.byteLength(data as Buffer);
    if (bytes === 0 || bytes > MAX_RECORD_BYTES) {
      closeRoom(source.claim.roomId, 4005, 'invalid record');
      return;
    }
    source.bytesForwarded += bytes;
    if (source.bytesForwarded > (options.bytesPerLease ?? DEFAULT_BYTES_PER_LEASE)) {
      closeRoom(source.claim.roomId, 4006, 'bandwidth limit');
      return;
    }
    const destination = peer(room, source.claim.role === 'host' ? 'controller' : 'host');
    if (!destination || destination.claim.purpose !== source.claim.purpose) {
      source.socket.send(JSON.stringify({ type: 'peer_unavailable' }));
      return;
    }
    if (destination.socket.bufferedAmount + bytes > MAX_BUFFERED_BYTES) {
      closeRoom(source.claim.roomId, 4006, 'backpressure limit');
      return;
    }
    destination.socket.send(data, { binary: true });
  }

  const heartbeat = setInterval(() => {
    for (const [roomId, room] of rooms) {
      for (const value of [room.host, room.controller]) {
        if (!value) continue;
        if (!value.alive) {
          closeRoom(roomId, 4000, 'heartbeat timeout');
          break;
        }
        value.alive = false;
        value.socket.ping();
      }
    }
  }, HEARTBEAT_MS);
  heartbeat.unref();

  return {
    server,
    listen: () => new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(options.port, options.host, () => {
        const address = server.address();
        if (!address || typeof address === 'string') reject(new Error('relay did not bind TCP'));
        else resolve(address.port);
      });
    }),
    drain: async () => {
      accepting = false;
      clearInterval(heartbeat);
      for (const roomId of [...rooms.keys()]) closeRoom(roomId, 1012, 'service restart');
      websocket.close();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}
