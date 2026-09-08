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

type Row = Record<string, unknown>;

const text = (value: unknown): string => String(value);
const nullableTimestamp = (value: unknown): string | null =>
  value instanceof Date ? value.toISOString() : value === null || value === undefined ? null : String(value);
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

function toOwnerInvitation(row: Row): OwnerInvitation {
  return {
    id: text(row.id), secretDigest: text(row.secret_digest), expiresAt: Number(row.expires_at),
    consumedAt: nullableTimestamp(row.consumed_at), createdAt: timestamp(row.created_at),
  };
}

function toRemoteEnrollment(row: Row): RemoteEnrollment {
  return {
    id: text(row.id), accountId: text(row.account_id), hostDeviceId: text(row.host_device_id),
    roomId: text(row.room_id), admissionDigest: text(row.admission_digest), expiresAt: Number(row.expires_at),
    provisionalDeviceId: row.provisional_device_id == null ? null : text(row.provisional_device_id),
    provisionalName: row.provisional_name == null ? null : text(row.provisional_name),
    provisionalPlatform: row.provisional_platform == null ? null : text(row.provisional_platform),
    provisionalPublicKey: row.provisional_public_key == null ? null : text(row.provisional_public_key),
    provisionalTokenDigest: row.provisional_token_digest == null ? null : text(row.provisional_token_digest),
    completedAt: nullableTimestamp(row.completed_at), cancelledAt: nullableTimestamp(row.cancelled_at),
    createdAt: timestamp(row.created_at),
  };
}

function toRemoteLink(row: Row): RemoteLink {
  return {
    id: text(row.id), accountId: text(row.account_id), hostDeviceId: text(row.host_device_id),
    clientDeviceId: text(row.client_device_id), roomId: text(row.room_id),
    grantRevision: Number(row.grant_revision), hostGeneration: Number(row.host_generation),
    controllerGeneration: Number(row.controller_generation), createdAt: timestamp(row.created_at),
  };
}

function toRemoteAdmission(row: Row): RemoteAdmission {
  return {
    id: text(row.id), linkId: row.link_id == null ? null : text(row.link_id),
    enrollmentId: row.enrollment_id == null ? null : text(row.enrollment_id), roomId: text(row.room_id),
    role: text(row.role) as RemoteAdmission['role'], purpose: text(row.purpose) as RemoteAdmission['purpose'],
    generation: Number(row.generation), expiresAt: Number(row.expires_at),
    attemptId: row.attempt_id == null ? null : text(row.attempt_id),
    leaseId: row.lease_id == null ? null : text(row.lease_id),
    leaseExpiresAt: row.lease_expires_at == null ? null : Number(row.lease_expires_at),
    createdAt: timestamp(row.created_at),
  };
}

function toRelayRevocation(row: Row): RelayRevocation {
  return {
    id: text(row.id), roomId: text(row.room_id), notAfter: Number(row.not_after),
    attempts: Number(row.attempts), acknowledgedAt: nullableTimestamp(row.acknowledged_at),
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
      `WITH updated AS (
         UPDATE accounts SET relay_enabled = $2 WHERE id = $1 RETURNING *
       ), revoked AS (
         INSERT INTO relay_revocation_outbox (id, room_id, not_after)
         SELECT 'rev_' || md5(room_id || clock_timestamp()::TEXT || random()::TEXT), room_id,
                EXTRACT(EPOCH FROM NOW())::BIGINT + 600
         FROM remote_links WHERE account_id = $1 AND $2 = FALSE
       ) SELECT * FROM updated`,
      [accountId, enabled],
    );
    return rows[0] ? toAccount(rows[0]) : null;
  }

  async createOwnerInvitation(input: CreateOwnerInvitationInput): Promise<OwnerInvitation> {
    const rows = await this.#query(
      `INSERT INTO owner_invitations (id, secret_digest, expires_at) VALUES ($1, $2, $3) RETURNING *`,
      [input.id, input.secretDigest, input.expiresAt],
    );
    return toOwnerInvitation(rows[0]!);
  }

  async consumeOwnerInvitation(id: string, secretDigest: string, consumedAt: string, now: number): Promise<boolean> {
    const rows = await this.#query(
      `UPDATE owner_invitations SET consumed_at = $3
       WHERE id = $1 AND secret_digest = $2 AND consumed_at IS NULL AND expires_at > $4 RETURNING id`,
      [id, secretDigest, consumedAt, now],
    );
    return rows.length === 1;
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
      await client.query(
        `INSERT INTO relay_revocation_outbox (id, room_id, not_after)
         SELECT 'rev_' || md5(room_id || clock_timestamp()::TEXT || random()::TEXT), room_id,
                EXTRACT(EPOCH FROM NOW())::BIGINT + 600
         FROM remote_links WHERE host_device_id = $1 OR client_device_id = $1`,
        [deviceId],
      );
      await client.query('DELETE FROM push_registrations WHERE device_id = $1', [deviceId]);
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
      `WITH previous AS (
         SELECT 1 FROM pairings WHERE host_device_id = $2 AND client_device_id = $3
       ), upserted AS (
         INSERT INTO pairings (account_id, host_device_id, client_device_id, permission)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (host_device_id, client_device_id)
         DO UPDATE SET permission = EXCLUDED.permission, revoked_at = NULL
         RETURNING *
       ), bumped AS (
         UPDATE remote_links SET grant_revision = grant_revision + 1,
           host_generation = host_generation + 1,
           controller_generation = controller_generation + 1
         WHERE host_device_id = $2 AND client_device_id = $3
           AND EXISTS (SELECT 1 FROM previous)
         RETURNING id
       )
       SELECT * FROM upserted`,
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
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const result = await client.query(
        `UPDATE pairings SET revoked_at = $3
         WHERE host_device_id = $1 AND client_device_id = $2 AND revoked_at IS NULL
         RETURNING host_device_id`, [hostDeviceId, clientDeviceId, revokedAt]);
      if (result.rows.length === 0) { await client.query('ROLLBACK'); return false; }
      await client.query(
        `INSERT INTO relay_revocation_outbox (id, room_id, not_after)
         SELECT 'rev_' || md5(room_id || clock_timestamp()::TEXT || random()::TEXT), room_id,
                EXTRACT(EPOCH FROM NOW())::BIGINT + 600
         FROM remote_links WHERE host_device_id = $1 AND client_device_id = $2`,
        [hostDeviceId, clientDeviceId]);
      await client.query('DELETE FROM push_registrations WHERE device_id = $1', [clientDeviceId]);
      await client.query('COMMIT');
      return true;
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally { client.release(); }
  }

  async createRemoteEnrollment(input: CreateRemoteEnrollmentInput): Promise<RemoteEnrollment> {
    const rows = await this.#query(
      `INSERT INTO remote_enrollments (id, account_id, host_device_id, room_id, admission_digest, expires_at)
       VALUES ($1,$2,$3,$4,$5,$6) RETURNING *`,
      [input.id, input.accountId, input.hostDeviceId, input.roomId, input.admissionDigest, input.expiresAt],
    );
    return toRemoteEnrollment(rows[0]!);
  }

  async getRemoteEnrollment(id: string, now: number): Promise<RemoteEnrollment | null> {
    const rows = await this.#query(
      `SELECT * FROM remote_enrollments WHERE id = $1 AND expires_at > $2 AND cancelled_at IS NULL`, [id, now]);
    return rows[0] ? toRemoteEnrollment(rows[0]) : null;
  }

  async claimRemoteEnrollment(input: ClaimRemoteEnrollmentInput): Promise<boolean> {
    const rows = await this.#query(
      `UPDATE remote_enrollments SET provisional_device_id = $3, provisional_name = $4,
         provisional_platform = $5, provisional_public_key = $6, provisional_token_digest = $7
       WHERE id = $1 AND admission_digest = $2 AND provisional_device_id IS NULL
         AND completed_at IS NULL AND cancelled_at IS NULL AND expires_at > $8 RETURNING id`,
      [input.id, input.admissionDigest, input.provisionalDeviceId, input.provisionalName,
       input.provisionalPlatform, input.provisionalPublicKey, input.provisionalTokenDigest, input.now]);
    return rows.length === 1;
  }

  async finalizeRemoteEnrollment(input: FinalizeRemoteEnrollmentInput): Promise<RemoteLink | null> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const selected = await client.query(
        `SELECT * FROM remote_enrollments
         WHERE id = $1 AND host_device_id = $2 AND completed_at IS NULL
           AND cancelled_at IS NULL AND expires_at > $3 FOR UPDATE`,
        [input.id, input.hostDeviceId, input.now],
      );
      const row = selected.rows[0] as Row | undefined;
      const enrollment = row ? toRemoteEnrollment(row) : null;
      const count = enrollment ? await client.query(
        'SELECT COUNT(*)::INT AS count FROM devices WHERE account_id = $1',
        [enrollment.accountId],
      ) : null;
      if (!enrollment || !enrollment.provisionalDeviceId || !enrollment.provisionalName ||
          !enrollment.provisionalPlatform || !enrollment.provisionalTokenDigest ||
          enrollment.provisionalPublicKey !== input.controllerPublicKey ||
          Number(count?.rows[0]?.count ?? input.maxDevices) >= input.maxDevices) {
        await client.query('ROLLBACK');
        return null;
      }
      await client.query(
        `INSERT INTO devices (id, account_id, name, platform, role, public_key, token_digest)
         VALUES ($1,$2,$3,$4,'client',$5,$6)`,
        [enrollment.provisionalDeviceId, enrollment.accountId, enrollment.provisionalName,
         enrollment.provisionalPlatform, input.controllerPublicKey, enrollment.provisionalTokenDigest],
      );
      await client.query(
        `INSERT INTO pairings (account_id, host_device_id, client_device_id, permission)
         VALUES ($1,$2,$3,$4)`,
        [enrollment.accountId, enrollment.hostDeviceId, enrollment.provisionalDeviceId, input.permission],
      );
      const linked = await client.query(
        `INSERT INTO remote_links (id, account_id, host_device_id, client_device_id, room_id)
         VALUES ($1,$2,$3,$4,$5) RETURNING *`,
        [input.linkId, enrollment.accountId, enrollment.hostDeviceId,
         enrollment.provisionalDeviceId, input.linkRoomId],
      );
      await client.query(
        'UPDATE remote_enrollments SET completed_at = $2 WHERE id = $1',
        [input.id, input.completedAt],
      );
      await client.query('COMMIT');
      return toRemoteLink(linked.rows[0] as Row);
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async cancelRemoteEnrollment(id: string, cancelledAt: string): Promise<boolean> {
    const rows = await this.#query(
      `WITH cancelled AS (
         UPDATE remote_enrollments SET cancelled_at = $2
         WHERE id = $1 AND completed_at IS NULL AND cancelled_at IS NULL RETURNING room_id
       ), queued AS (
         INSERT INTO relay_revocation_outbox (id, room_id, not_after)
         SELECT 'rev_' || md5(room_id || clock_timestamp()::TEXT || random()::TEXT), room_id,
                EXTRACT(EPOCH FROM NOW())::BIGINT + 600 FROM cancelled
       ) SELECT room_id FROM cancelled`, [id, cancelledAt]);
    return rows.length === 1;
  }

  async getOrCreateRemoteLink(accountId: string, hostDeviceId: string, clientDeviceId: string, roomId: string): Promise<RemoteLink> {
    const rows = await this.#query(
      `INSERT INTO remote_links (id, account_id, host_device_id, client_device_id, room_id)
       VALUES ($1,$2,$3,$4,$5)
       ON CONFLICT (host_device_id, client_device_id) DO UPDATE SET account_id = EXCLUDED.account_id
       RETURNING *`,
      [`link_${roomId}`, accountId, hostDeviceId, clientDeviceId, roomId]);
    return toRemoteLink(rows[0]!);
  }

  async getRemoteLink(id: string): Promise<RemoteLink | null> {
    const rows = await this.#query('SELECT * FROM remote_links WHERE id = $1', [id]);
    return rows[0] ? toRemoteLink(rows[0]) : null;
  }

  async createRemoteAdmission(input: CreateRemoteAdmissionInput): Promise<RemoteAdmission> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const result = await client.query(
        `INSERT INTO remote_admissions
         (id, link_id, enrollment_id, room_id, role, purpose, generation, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *`,
        [input.id,input.linkId,input.enrollmentId,input.roomId,input.role,input.purpose,input.generation,input.expiresAt]);
      if (input.linkId) {
        const column = input.role === 'host' ? 'host_generation' : 'controller_generation';
        await client.query(`UPDATE remote_links SET ${column} = $2 WHERE id = $1`, [input.linkId, input.generation]);
      }
      await client.query('COMMIT');
      return toRemoteAdmission(result.rows[0] as Row);
    } catch (error) { await client.query('ROLLBACK'); throw error; } finally { client.release(); }
  }

  async redeemRemoteAdmission(id: string, attemptId: string, leaseId: string, leaseExpiresAt: number, now: number): Promise<RemoteAdmission | null> {
    const rows = await this.#query(
      `UPDATE remote_admissions SET attempt_id = $2, lease_id = $3, lease_expires_at = $4
       WHERE id = $1 AND expires_at > $5 AND attempt_id IS NULL RETURNING *`,
      [id, attemptId, leaseId, leaseExpiresAt, now]);
    if (rows[0]) return toRemoteAdmission(rows[0]);
    const existing = await this.getRemoteAdmission(id);
    return existing?.attemptId === attemptId && existing.expiresAt > now ? existing : null;
  }

  async getRemoteAdmission(id: string): Promise<RemoteAdmission | null> {
    const rows = await this.#query('SELECT * FROM remote_admissions WHERE id = $1', [id]);
    return rows[0] ? toRemoteAdmission(rows[0]) : null;
  }

  async getRemoteAdmissionByLease(leaseId: string): Promise<RemoteAdmission | null> {
    const rows = await this.#query('SELECT * FROM remote_admissions WHERE lease_id = $1', [leaseId]);
    return rows[0] ? toRemoteAdmission(rows[0]) : null;
  }

  async extendRemoteLease(leaseId: string, expiresAt: number, now: number): Promise<RemoteAdmission | null> {
    const rows = await this.#query(
      `UPDATE remote_admissions SET lease_expires_at = $2
       WHERE lease_id = $1 AND lease_expires_at > $3 RETURNING *`, [leaseId, expiresAt, now]);
    return rows[0] ? toRemoteAdmission(rows[0]) : null;
  }

  async listPendingRelayRevocations(limit: number): Promise<RelayRevocation[]> {
    const rows = await this.#query(
      `UPDATE relay_revocation_outbox SET attempts = attempts + 1
       WHERE id IN (SELECT id FROM relay_revocation_outbox WHERE acknowledged_at IS NULL ORDER BY created_at LIMIT $1)
       RETURNING *`, [limit]);
    return rows.map(toRelayRevocation);
  }

  async acknowledgeRelayRevocation(id: string, acknowledgedAt: string): Promise<void> {
    await this.#query('UPDATE relay_revocation_outbox SET acknowledged_at = $2 WHERE id = $1', [id, acknowledgedAt]);
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

  async upsertPushRegistration(deviceId: string, pushToken: string, updatedAt: string): Promise<void> {
    await this.#query(
      `INSERT INTO push_registrations (device_id, push_token, updated_at) VALUES ($1, $2, $3)
       ON CONFLICT (device_id) DO UPDATE SET push_token = EXCLUDED.push_token, updated_at = EXCLUDED.updated_at`,
      [deviceId, pushToken, updatedAt],
    );
  }

  async getPushRegistration(deviceId: string): Promise<PushRegistration | null> {
    const rows = await this.#query('SELECT * FROM push_registrations WHERE device_id = $1', [deviceId]);
    const row = rows[0];
    return row
      ? { deviceId: text(row.device_id), pushToken: text(row.push_token), updatedAt: timestamp(row.updated_at) }
      : null;
  }

  async deletePushRegistration(deviceId: string): Promise<boolean> {
    const rows = await this.#query('DELETE FROM push_registrations WHERE device_id = $1 RETURNING 1', [deviceId]);
    return rows.length > 0;
  }

  async recordAttentionEvent(
    hostDeviceId: string,
    clientDeviceId: string,
    eventId: string,
    createdAt: string,
  ): Promise<boolean> {
    const rows = await this.#query(
      `INSERT INTO attention_events (host_device_id, client_device_id, event_id, created_at)
       VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING 1`,
      [hostDeviceId, clientDeviceId, eventId, createdAt],
    );
    return rows.length > 0;
  }

  async purgeExpired(now: number): Promise<number> {
    const attention = await this.#query(
      'DELETE FROM attention_events WHERE created_at <= to_timestamp($1) RETURNING 1',
      [now - 24 * 60 * 60],
    );
    const admissions = await this.#query(
      'DELETE FROM remote_admissions WHERE expires_at <= $1 AND COALESCE(lease_expires_at, 0) <= $1 RETURNING 1',
      [now],
    );
    const enrollments = await this.#query(
      'DELETE FROM remote_enrollments WHERE expires_at <= $1 AND completed_at IS NULL RETURNING 1',
      [now],
    );
    return enrollments.length + admissions.length + attention.length;
  }
}
