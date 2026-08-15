/**
 * Environment configuration for the control plane.
 *
 * The service is deployed independently (Railway) from the relay, so every
 * knob is an environment variable and every invalid value fails fast at boot
 * rather than at the first request. Nothing here may hold a Latch gateway
 * token: the control plane never learns one.
 */

export interface Config {
  /** TCP port for the HTTP listener. Railway injects `PORT`. */
  readonly port: number;
  /** Interface to bind. Railway requires a non-loopback bind. */
  readonly host: string;
  /** PostgreSQL connection string. */
  readonly databaseUrl: string;
  /** Maximum pooled PostgreSQL connections. */
  readonly databasePoolSize: number;
  /** Whether TLS certificate verification is relaxed for the managed database. */
  readonly databaseSslRejectUnauthorized: boolean;
  /** Run pending migrations during boot. */
  readonly migrateOnBoot: boolean;
  /** Public base URL of the separately deployed relay. */
  readonly relayUrl: string;
  /**
   * Shared secret presented by the relay deployable when it asks the control
   * plane to authorize an endpoint admission. Never given to a device.
   */
  readonly relayServiceToken: string | null;
  /** Presence/candidate lifetime in seconds. Mirrors the local 90s window. */
  readonly presenceTtlSeconds: number;
  /** Relay ticket lifetime in seconds. Mirrors the local 60s window. */
  readonly relayTicketTtlSeconds: number;
  /** Rendezvous offer lifetime in seconds. */
  readonly rendezvousTtlSeconds: number;
  /** Maximum devices enrolled per account. */
  readonly maxDevicesPerAccount: number;
  /** Maximum direct candidates accepted in one presence or rendezvous body. */
  readonly maxCandidates: number;
  /** Requests allowed per device inside one rate window. */
  readonly rateLimitPerMinute: number;
  /** Deployment identifier surfaced by the health endpoints. */
  readonly releaseId: string;
  /** Environment label used in health output only. */
  readonly environment: string;
}

export class ConfigError extends Error {}

type Env = Record<string, string | undefined>;

function required(env: Env, key: string): string {
  const value = env[key]?.trim();
  if (!value) {
    throw new ConfigError(`${key} is required`);
  }
  return value;
}

function optional(env: Env, key: string, fallback: string): string {
  const value = env[key]?.trim();
  return value ? value : fallback;
}

function integer(env: Env, key: string, fallback: number, min: number, max: number): number {
  const raw = env[key]?.trim();
  if (!raw) {
    return fallback;
  }
  const value = Number(raw);
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new ConfigError(`${key} must be an integer between ${min} and ${max}`);
  }
  return value;
}

function boolean(env: Env, key: string, fallback: boolean): boolean {
  const raw = env[key]?.trim().toLowerCase();
  if (!raw) {
    return fallback;
  }
  if (raw === 'true' || raw === '1' || raw === 'yes') {
    return true;
  }
  if (raw === 'false' || raw === '0' || raw === 'no') {
    return false;
  }
  throw new ConfigError(`${key} must be a boolean`);
}

/**
 * Reads and validates configuration. Callers pass `process.env` in production
 * and a literal object in tests.
 */
export function loadConfig(env: Env): Config {
  const relayServiceToken = env.RELAY_SERVICE_TOKEN?.trim() ?? '';
  if (relayServiceToken && relayServiceToken.length < 32) {
    throw new ConfigError('RELAY_SERVICE_TOKEN must be at least 32 characters');
  }
  return {
    port: integer(env, 'PORT', 8080, 1, 65_535),
    host: optional(env, 'HOST', '0.0.0.0'),
    databaseUrl: required(env, 'DATABASE_URL'),
    databasePoolSize: integer(env, 'DATABASE_POOL_SIZE', 10, 1, 100),
    databaseSslRejectUnauthorized: boolean(env, 'DATABASE_SSL_REJECT_UNAUTHORIZED', false),
    migrateOnBoot: boolean(env, 'MIGRATE_ON_BOOT', true),
    relayUrl: optional(env, 'RELAY_URL', ''),
    relayServiceToken: relayServiceToken ? relayServiceToken : null,
    presenceTtlSeconds: integer(env, 'PRESENCE_TTL_SECONDS', 90, 10, 300),
    relayTicketTtlSeconds: integer(env, 'RELAY_TICKET_TTL_SECONDS', 60, 10, 300),
    rendezvousTtlSeconds: integer(env, 'RENDEZVOUS_TTL_SECONDS', 60, 10, 300),
    maxDevicesPerAccount: integer(env, 'MAX_DEVICES_PER_ACCOUNT', 32, 2, 256),
    maxCandidates: integer(env, 'MAX_CANDIDATES', 8, 1, 32),
    rateLimitPerMinute: integer(env, 'RATE_LIMIT_PER_MINUTE', 240, 10, 10_000),
    releaseId: optional(env, 'RAILWAY_GIT_COMMIT_SHA', 'development'),
    environment: optional(env, 'RAILWAY_ENVIRONMENT_NAME', 'local'),
  };
}
