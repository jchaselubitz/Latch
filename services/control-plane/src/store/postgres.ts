/**
 * PostgreSQL store.
 *
 * Every method mirrors MemoryStore exactly; the difference is durability and
 * the fact that expiry-sensitive reads are filtered in SQL so an expired row
 * can never be served even before the sweeper removes it.
 */

import pg from 'pg';

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

type Row = Record<string, unknown>;

const text = (value: unknown): string => String(value);
const nullableTimestamp = (value: unknown): string | null =>
  value instanceof Date ? value.toISOString() : value === null || value === undefined ? null : String(value);
const optionalText = (value: unknown): string | undefined =>
  value === null || value === undefined ? undefined : String(value);
const timestamp = (value: unknown): string => nullableTimestamp(value) ?? new Date(0).toISOString();

function toAccount(row: Row): Account {
  return {
    id: text(row.id),
    label: text(row.label),
    relayEnabled: Boolean(row.relay_enabled),
    createdAt: timestamp(row.created_at),
  };
}

function toDevice(row: Row): Device {
  return {
    id: text(row.id),
    accountId: text(row.account_id),
    name: text(row.name),
    platform: text(row.platform),
    role: text(row.role) as DeviceRole,
    publicKey: text(row.public_key),
    keyGeneration: Number(row.key_generation),
    revokedAt: nullableTimestamp(row.revoked_at),
    lastSeenAt: nullableTimestamp(row.last_seen_at),
    createdAt: timestamp(row.created_at),
  };
}

function toPairing(row: Row): Pairing {
  return {
    accountId: text(row.account_id),
    hostDeviceId: text(row.host_device_id),
    clientDeviceId: text(row.client_device_id),
    permission: text(row.permission) as Permission,
    createdAt: timestamp(row.created_at),
    revokedAt: nullableTimestamp(row.revoked_at),
  };
}

function toPairingRequest(row: Row): PairingRequest {
  return {
    pairingId: text(row.pairing_id),
    accountId: text(row.account_id),
    hostDeviceId: text(row.host_device_id),
    secretDigest: text(row.secret_digest),
    phrase: row.phrase === null || row.phrase === undefined ? null : text(row.phrase),
    permission: text(row.permission) as Permission,
    expiresAt: Number(row.expires_at),
    consumedAt: nullableTimestamp(row.consumed_at),
    createdAt: timestamp(row.created_at),
  };
}

function toCandidates(value: unknown): Candidate[] {
  const parsed = typeof value === 'string' ? JSON.parse(value) : value;
  return Array.isArray(parsed) ? (parsed as Candidate[]) : [];
}

function toPresence(row: Row): Presence {
  return {
    deviceId: text(row.device_id),
    accountId: text(row.account_id),
    candidates: toCandidates(row.candidates),
    iceUfrag: optionalText(row.ice_ufrag),
    icePwd: optionalText(row.ice_pwd),
    expiresAt: Number(row.expires_at),
    updatedAt: timestamp(row.updated_at),
  };
}

function toOffer(row: Row): RendezvousOffer {
  return {
    id: text(row.id),
    accountId: text(row.account_id),
    requesterDeviceId: text(row.requester_device_id),
    targetDeviceId: text(row.target_device_id),
    requestId: text(row.request_id),
    candidates: toCandidates(row.candidates),
    iceUfrag: optionalText(row.ice_ufrag),
    icePwd: optionalText(row.ice_pwd),
    expiresAt: Number(row.expires_at),
    createdAt: timestamp(row.created_at),
  };
}

function toTicket(row: Row): RelayTicket {
  const admitted = typeof row.admitted_device_ids === 'string'
    ? JSON.parse(row.admitted_device_ids)
    : row.admitted_device_ids;
  return {
    relayId: text(row.relay_id),
    accountId: text(row.account_id),
    hostDeviceId: text(row.host_device_id),
    clientDeviceId: text(row.client_device_id),
    secretDigest: text(row.secret_digest),
    expiresAt: Number(row.expires_at),
    admittedDeviceIds: Array.isArray(admitted) ? (admitted as string[]) : [],
    createdAt: timestamp(row.created_at),
  };
}

export interface PostgresStoreOptions {
  readonly connectionString: string;
  readonly poolSize: number;
  readonly sslRejectUnauthorized: boolean;
}

export class PostgresStore implements Store {
  readonly pool: pg.Pool;

  constructor(options: PostgresStoreOptions) {
    const local = /localhost|127\.0\.0\.1|\.railway\.internal/.test(options.connectionString);
    this.pool = new pg.Pool({
      connectionString: options.connectionString,
      max: options.poolSize,
      // Railway's managed PostgreSQL presents a certificate signed by an
      // internal authority; private-network connections are not exposed to
      // the internet in the first place.
      ssl: local ? false : { rejectUnauthorized: options.sslRejectUnauthorized },
    });
  }

  async #query(sql: string, values: unknown[] = []): Promise<Row[]> {
    const result = await this.pool.query(sql, values);
    return result.rows as Row[];
  }

  async ping(): Promise<void> {
    await this.#query('SELECT 1');
  }

  async close(): Promise<void> {
    await this.pool.end();
  }

  async createAccount(input: CreateAccountInput): Promise<Account> {
    const rows = await this.#query(
      `INSERT INTO accounts (id, label, token_digest) VALUES ($1, $2, $3) RETURNING *`,
      [input.id, input.label, input.tokenDigest],
    );
    return toAccount(rows[0]!);
  }

  async getAccount(accountId: string): Promise<Account | null> {
    const rows = await this.#query('SELECT * FROM accounts WHERE id = $1', [accountId]);
    return rows[0] ? toAccount(rows[0]) : null;
  }

  async setRelayEnabled(accountId: string, enabled: boolean): Promise<Account | null> {
    const rows = await this.#query(
      'UPDATE accounts SET relay_enabled = $2 WHERE id = $1 RETURNING *',
      [accountId, enabled],
    );
    return rows[0] ? toAccount(rows[0]) : null;
  }

  async accountByTokenDigest(accountId: string, tokenDigest: string): Promise<Account | null> {
    const rows = await this.#query('SELECT * FROM accounts WHERE id = $1', [accountId]);
    const row = rows[0];
    if (!row || !digestsMatch(text(row.token_digest), tokenDigest)) {
      return null;
    }
    return toAccount(row);
  }

  async createDevice(input: CreateDeviceInput): Promise<Device> {
    const rows = await this.#query(
      `INSERT INTO devices (id, account_id, name, platform, role, public_key, token_digest)
       VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *`,
      [
        input.id,
        input.accountId,
        input.name,
        input.platform,
        input.role,
        input.publicKey,
        input.tokenDigest,
      ],
    );
    return toDevice(rows[0]!);
  }

  async getDevice(deviceId: string): Promise<Device | null> {
    const rows = await this.#query('SELECT * FROM devices WHERE id = $1', [deviceId]);
    return rows[0] ? toDevice(rows[0]) : null;
  }

  async listDevices(accountId: string): Promise<Device[]> {
    const rows = await this.#query(
      'SELECT * FROM devices WHERE account_id = $1 ORDER BY created_at ASC',
      [accountId],
    );
    return rows.map(toDevice);
  }

  async countDevices(accountId: string): Promise<number> {
    const rows = await this.#query(
      'SELECT COUNT(*)::INT AS count FROM devices WHERE account_id = $1',
      [accountId],
    );
    return Number(rows[0]?.count ?? 0);
  }

  async deviceByTokenDigest(deviceId: string, tokenDigest: string): Promise<Device | null> {
    const rows = await this.#query('SELECT * FROM devices WHERE id = $1 AND revoked_at IS NULL', [
      deviceId,
    ]);
    const row = rows[0];
    if (!row || !digestsMatch(text(row.token_digest), tokenDigest)) {
      return null;
    }
    return toDevice(row);
  }

  async touchDevice(deviceId: string, seenAt: string): Promise<void> {
    await this.#query('UPDATE devices SET last_seen_at = $2 WHERE id = $1', [deviceId, seenAt]);
  }

  async rotateDeviceKey(deviceId: string, publicKey: string): Promise<Device | null> {
    const rows = await this.#query(
      `UPDATE devices SET public_key = $2, key_generation = key_generation + 1
       WHERE id = $1 AND revoked_at IS NULL RETURNING *`,
      [deviceId, publicKey],
    );
    return rows[0] ? toDevice(rows[0]) : null;
  }

  async revokeDevice(deviceId: string, revokedAt: string): Promise<Device | null> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const revoked = await client.query(
        `UPDATE devices SET revoked_at = COALESCE(revoked_at, $2) WHERE id = $1 RETURNING *`,
        [deviceId, revokedAt],
      );
      if (revoked.rows.length === 0) {
        await client.query('ROLLBACK');
        return null;
      }
      await client.query(
        `UPDATE pairings SET revoked_at = $2
         WHERE revoked_at IS NULL AND (host_device_id = $1 OR client_device_id = $1)`,
        [deviceId, revokedAt],
      );
      await client.query('DELETE FROM presence WHERE device_id = $1', [deviceId]);
      await client.query(
        'DELETE FROM rendezvous_offers WHERE requester_device_id = $1 OR target_device_id = $1',
        [deviceId],
      );
      await client.query(
        'DELETE FROM relay_tickets WHERE host_device_id = $1 OR client_device_id = $1',
        [deviceId],
      );
      await client.query('DELETE FROM pairing_requests WHERE host_device_id = $1', [deviceId]);
      await client.query('COMMIT');
      return toDevice(revoked.rows[0] as Row);
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async upsertPairing(
    accountId: string,
    hostDeviceId: string,
    clientDeviceId: string,
    permission: Permission,
  ): Promise<Pairing> {
    const rows = await this.#query(
      `INSERT INTO pairings (account_id, host_device_id, client_device_id, permission)
       VALUES ($1, $2, $3, $4)
       ON CONFLICT (host_device_id, client_device_id)
       DO UPDATE SET permission = EXCLUDED.permission, revoked_at = NULL
       RETURNING *`,
      [accountId, hostDeviceId, clientDeviceId, permission],
    );
    return toPairing(rows[0]!);
  }

  async getPairing(hostDeviceId: string, clientDeviceId: string): Promise<Pairing | null> {
    const rows = await this.#query(
      `SELECT * FROM pairings
       WHERE host_device_id = $1 AND client_device_id = $2 AND revoked_at IS NULL`,
      [hostDeviceId, clientDeviceId],
    );
    return rows[0] ? toPairing(rows[0]) : null;
  }

  async listPairingsForDevice(deviceId: string): Promise<Pairing[]> {
    const rows = await this.#query(
      `SELECT * FROM pairings
       WHERE revoked_at IS NULL AND (host_device_id = $1 OR client_device_id = $1)
       ORDER BY created_at ASC`,
      [deviceId],
    );
    return rows.map(toPairing);
  }

  async revokePairing(
    hostDeviceId: string,
    clientDeviceId: string,
    revokedAt: string,
  ): Promise<boolean> {
    const rows = await this.#query(
      `UPDATE pairings SET revoked_at = $3
       WHERE host_device_id = $1 AND client_device_id = $2 AND revoked_at IS NULL
       RETURNING host_device_id`,
      [hostDeviceId, clientDeviceId, revokedAt],
    );
    if (rows.length === 0) {
      return false;
    }
    await this.#query(
      'DELETE FROM relay_tickets WHERE host_device_id = $1 AND client_device_id = $2',
      [hostDeviceId, clientDeviceId],
    );
    return true;
  }

  async createPairingRequest(input: CreatePairingRequestInput): Promise<PairingRequest> {
    const rows = await this.#query(
      `INSERT INTO pairing_requests
         (pairing_id, account_id, host_device_id, secret_digest, phrase, permission, expires_at)
       VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *`,
      [
        input.pairingId,
        input.accountId,
        input.hostDeviceId,
        input.secretDigest,
        input.phrase,
        input.permission,
        input.expiresAt,
      ],
    );
    return toPairingRequest(rows[0]!);
  }

  async getPairingRequest(pairingId: string, now: number): Promise<PairingRequest | null> {
    const rows = await this.#query(
      `SELECT * FROM pairing_requests
       WHERE pairing_id = $1 AND consumed_at IS NULL AND expires_at > $2`,
      [pairingId, now],
    );
    return rows[0] ? toPairingRequest(rows[0]) : null;
  }

  async consumePairingRequest(
    pairingId: string,
    consumedAt: string,
    now: number,
  ): Promise<boolean> {
    // Conditional update: two phones scanning the same code cannot both win.
    const rows = await this.#query(
      `UPDATE pairing_requests SET consumed_at = $2
       WHERE pairing_id = $1 AND consumed_at IS NULL AND expires_at > $3
       RETURNING pairing_id`,
      [pairingId, consumedAt, now],
    );
    return rows.length > 0;
  }

  async countPendingPairingRequests(hostDeviceId: string, now: number): Promise<number> {
    const rows = await this.#query(
      `SELECT COUNT(*)::INT AS count FROM pairing_requests
       WHERE host_device_id = $1 AND consumed_at IS NULL AND expires_at > $2`,
      [hostDeviceId, now],
    );
    return Number(rows[0]?.count ?? 0);
  }

  async publishPresence(input: PublishPresenceInput): Promise<Presence> {
    const rows = await this.#query(
      `INSERT INTO presence (device_id, account_id, candidates, ice_ufrag, ice_pwd, expires_at, updated_at)
       VALUES ($1, $2, $3::JSONB, $4, $5, $6, NOW())
       ON CONFLICT (device_id) DO UPDATE
         SET candidates = EXCLUDED.candidates,
             ice_ufrag = EXCLUDED.ice_ufrag,
             ice_pwd = EXCLUDED.ice_pwd,
             expires_at = EXCLUDED.expires_at,
             updated_at = NOW()
       RETURNING *`,
      [input.deviceId, input.accountId, JSON.stringify(input.candidates), input.iceUfrag ?? null, input.icePwd ?? null, input.expiresAt],
    );
    return toPresence(rows[0]!);
  }

  async getPresence(deviceId: string, now: number): Promise<Presence | null> {
    const rows = await this.#query(
      'SELECT * FROM presence WHERE device_id = $1 AND expires_at > $2',
      [deviceId, now],
    );
    return rows[0] ? toPresence(rows[0]) : null;
  }

  async clearPresence(deviceId: string): Promise<void> {
    await this.#query('DELETE FROM presence WHERE device_id = $1', [deviceId]);
  }

  async createOffer(input: CreateOfferInput): Promise<RendezvousOffer> {
    const rows = await this.#query(
      `INSERT INTO rendezvous_offers
         (id, account_id, requester_device_id, target_device_id, request_id, candidates, ice_ufrag, ice_pwd, expires_at)
       VALUES ($1, $2, $3, $4, $5, $6::JSONB, $7, $8, $9) RETURNING *`,
      [
        input.id,
        input.accountId,
        input.requesterDeviceId,
        input.targetDeviceId,
        input.requestId,
        JSON.stringify(input.candidates),
        input.iceUfrag ?? null,
        input.icePwd ?? null,
        input.expiresAt,
      ],
    );
    return toOffer(rows[0]!);
  }

  async takeOffers(targetDeviceId: string, now: number): Promise<RendezvousOffer[]> {
    const rows = await this.#query(
      `DELETE FROM rendezvous_offers
       WHERE target_device_id = $1 AND expires_at > $2
       RETURNING *`,
      [targetDeviceId, now],
    );
    await this.#query('DELETE FROM rendezvous_offers WHERE expires_at <= $1', [now]);
    return rows.map(toOffer).sort((left, right) => left.createdAt.localeCompare(right.createdAt));
  }

  async createRelayTicket(input: CreateRelayTicketInput): Promise<RelayTicket> {
    const rows = await this.#query(
      `INSERT INTO relay_tickets
         (relay_id, account_id, host_device_id, client_device_id, secret_digest, expires_at)
       VALUES ($1, $2, $3, $4, $5, $6) RETURNING *`,
      [
        input.relayId,
        input.accountId,
        input.hostDeviceId,
        input.clientDeviceId,
        input.secretDigest,
        input.expiresAt,
      ],
    );
    return toTicket(rows[0]!);
  }

  async getRelayTicket(relayId: string, now: number): Promise<RelayTicket | null> {
    const rows = await this.#query(
      'SELECT * FROM relay_tickets WHERE relay_id = $1 AND expires_at > $2',
      [relayId, now],
    );
    return rows[0] ? toTicket(rows[0]) : null;
  }

  async admitToRelayTicket(
    relayId: string,
    deviceId: string,
    now: number,
  ): Promise<boolean | null> {
    // The admission list is appended conditionally in one statement so two
    // concurrent relay endpoints cannot both claim the same slot.
    const rows = await this.#query(
      `UPDATE relay_tickets
       SET admitted_device_ids = admitted_device_ids || to_jsonb($2::TEXT)
       WHERE relay_id = $1
         AND expires_at > $3
         AND NOT (admitted_device_ids @> to_jsonb($2::TEXT))
       RETURNING relay_id`,
      [relayId, deviceId, now],
    );
    if (rows.length > 0) {
      return true;
    }
    const existing = await this.getRelayTicket(relayId, now);
    if (!existing) {
      return null;
    }
    return false;
  }

  async createTurnCredential(input: CreateTurnCredentialInput): Promise<void> {
    await this.#query(
      `INSERT INTO turn_credentials (username, account_id, device_id, expires_at)
       VALUES ($1, $2, $3, $4) ON CONFLICT (username) DO NOTHING`,
      [input.username, input.accountId, input.deviceId, input.expiresAt],
    );
  }

  async takeTurnCredentialUsernames(deviceIds: readonly string[], now: number): Promise<string[]> {
    if (deviceIds.length === 0) return [];
    const rows = await this.#query(
      `DELETE FROM turn_credentials WHERE expires_at <= $2 OR device_id = ANY($1::TEXT[])
       RETURNING username, expires_at`, [deviceIds, now],
    );
    return rows.filter((row) => Number(row.expires_at) > now).map((row) => text(row.username));
  }

  async takeTurnCredentialUsernamesForAccount(accountId: string, now: number): Promise<string[]> {
    const rows = await this.#query(
      `DELETE FROM turn_credentials WHERE expires_at <= $2 OR account_id = $1 RETURNING username, expires_at`,
      [accountId, now],
    );
    return rows.filter((row) => Number(row.expires_at) > now).map((row) => text(row.username));
  }

  async recordAccessEvent(event: AccessEvent): Promise<void> {
    await this.#query(
      `INSERT INTO access_events (account_id, device_id, action, result, created_at)
       VALUES ($1, $2, $3, $4, $5)`,
      [event.accountId, event.deviceId, event.action, event.result, event.createdAt],
    );
  }

  async listAccessEvents(accountId: string, limit: number): Promise<AccessEvent[]> {
    const rows = await this.#query(
      `SELECT account_id, device_id, action, result, created_at
       FROM access_events WHERE account_id = $1 ORDER BY id DESC LIMIT $2`,
      [accountId, limit],
    );
    return rows.map((row) => ({
      accountId: row.account_id === null ? null : text(row.account_id),
      deviceId: row.device_id === null ? null : text(row.device_id),
      action: text(row.action),
      result: text(row.result) as AccessEvent['result'],
      createdAt: timestamp(row.created_at),
    }));
  }

  async purgeExpired(now: number): Promise<number> {
    const presence = await this.#query('DELETE FROM presence WHERE expires_at <= $1 RETURNING 1', [
      now,
    ]);
    const offers = await this.#query(
      'DELETE FROM rendezvous_offers WHERE expires_at <= $1 RETURNING 1',
      [now],
    );
    const tickets = await this.#query(
      'DELETE FROM relay_tickets WHERE expires_at <= $1 RETURNING 1',
      [now],
    );
    const pairingRequests = await this.#query(
      'DELETE FROM pairing_requests WHERE expires_at <= $1 RETURNING 1',
      [now],
    );
    const turnCredentials = await this.#query('DELETE FROM turn_credentials WHERE expires_at <= $1 RETURNING 1', [now]);
    return presence.length + offers.length + tickets.length + pairingRequests.length + turnCredentials.length;
  }
}
