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
  /** Maximum devices enrolled per account. */
  readonly maxDevicesPerAccount: number;
  /** Requests allowed per device inside one rate window. */
  readonly rateLimitPerMinute: number;
  /** Remote-link admissions allowed per device in one minute. */
  readonly admissionRatePerDevice: number;
  /** Remote-link admissions allowed across one owner account in one minute. */
  readonly admissionRatePerOwner: number;
  /** Remote-link admissions allowed from one source address in one minute. */
  readonly admissionRatePerIp: number;
  /** Remote-link admissions allowed by this deployment in one minute. */
  readonly admissionRateGlobal: number;
  /** Trust the hosting edge's first X-Forwarded-For value for source budgets. */
  readonly trustProxy: boolean;
  /** Deployment identifier surfaced by the health endpoints. */
  readonly releaseId: string;
  /** Environment label used in health output only. */
  readonly environment: string;
  /** Operator credential that alone may mint the owner's bootstrap invitation. */
  readonly operatorSecret: string | null;
  /** Ed25519 PKCS#8 PEM used for short-lived relay claims. */
  readonly admissionPrivateKeyPem: string | null;
  /** Public key identifier pinned by the relay. */
  readonly admissionKeyId: string;
  /** Exact claim issuer pinned by the relay. */
  readonly admissionIssuer: string;
  /** Exact externally reachable relay WebSocket URL. */
  readonly relayUrl: string;
  /** Credential accepted only by the private relay redemption API. */
  readonly relayServiceToken: string | null;
  /** Credential used by the durable outbox worker to invalidate relay rooms. */
  readonly relayInvalidationSecret: string | null;
  /** APNs token-based authentication, or null when notifications are off. */
  readonly apns: ApnsSettings | null;
  /** Attention notifications a host may submit per minute. */
  readonly attentionRatePerHost: number;
}

export interface ApnsSettings {
  readonly keyId: string;
  readonly teamId: string;
  readonly privateKeyPem: string;
  readonly topic: string;
  readonly environment: 'sandbox' | 'production';
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
  const operatorSecret = env.OPERATOR_SECRET?.trim() ?? '';
  const admissionPrivateKeyPem = env.ADMISSION_PRIVATE_KEY_PEM?.replace(/\\n/g, '\n').trim() ?? '';
  const relayServiceToken = env.RELAY_SERVICE_TOKEN?.trim() ?? '';
  const relayInvalidationSecret = env.RELAY_INVALIDATION_SECRET?.trim() ?? '';
  for (const [name, value] of [
    ['OPERATOR_SECRET', operatorSecret],
    ['RELAY_SERVICE_TOKEN', relayServiceToken],
    ['RELAY_INVALIDATION_SECRET', relayInvalidationSecret],
  ] as const) {
    if (value && value.length < 32) throw new ConfigError(`${name} must be at least 32 characters`);
  }
  const apnsKeyId = env.APNS_KEY_ID?.trim() ?? '';
  const apnsTeamId = env.APNS_TEAM_ID?.trim() ?? '';
  const apnsPrivateKeyPem = env.APNS_PRIVATE_KEY_PEM?.replace(/\\n/g, '\n').trim() ?? '';
  const apnsEnvironment = optional(env, 'APNS_ENVIRONMENT', 'sandbox');
  if (apnsEnvironment !== 'sandbox' && apnsEnvironment !== 'production') {
    throw new ConfigError('APNS_ENVIRONMENT must be sandbox or production');
  }
  const apnsPartial = [apnsKeyId, apnsTeamId, apnsPrivateKeyPem].filter(Boolean).length;
  if (apnsPartial !== 0 && apnsPartial !== 3) {
    throw new ConfigError('APNS_KEY_ID, APNS_TEAM_ID, and APNS_PRIVATE_KEY_PEM must be set together');
  }
  if (apnsKeyId && !/^[A-Z0-9]{10}$/.test(apnsKeyId)) throw new ConfigError('APNS_KEY_ID must be a 10-character key identifier');
  if (apnsTeamId && !/^[A-Z0-9]{10}$/.test(apnsTeamId)) throw new ConfigError('APNS_TEAM_ID must be a 10-character team identifier');
  return {
    port: integer(env, 'PORT', 8080, 1, 65_535),
    host: optional(env, 'HOST', '0.0.0.0'),
    databaseUrl: required(env, 'DATABASE_URL'),
    databasePoolSize: integer(env, 'DATABASE_POOL_SIZE', 10, 1, 100),
    databaseSslRejectUnauthorized: boolean(env, 'DATABASE_SSL_REJECT_UNAUTHORIZED', false),
    migrateOnBoot: boolean(env, 'MIGRATE_ON_BOOT', true),
    maxDevicesPerAccount: integer(env, 'MAX_DEVICES_PER_ACCOUNT', 32, 2, 256),
    rateLimitPerMinute: integer(env, 'RATE_LIMIT_PER_MINUTE', 240, 10, 10_000),
    admissionRatePerDevice: integer(env, 'REMOTE_ADMISSION_RATE_PER_DEVICE', 60, 4, 1_000),
    admissionRatePerOwner: integer(env, 'REMOTE_ADMISSION_RATE_PER_OWNER', 180, 8, 5_000),
    admissionRatePerIp: integer(env, 'REMOTE_ADMISSION_RATE_PER_IP', 120, 4, 5_000),
    admissionRateGlobal: integer(env, 'REMOTE_ADMISSION_RATE_GLOBAL', 1_000, 16, 100_000),
    trustProxy: boolean(env, 'TRUST_PROXY', false),
    releaseId: optional(env, 'RAILWAY_GIT_COMMIT_SHA', 'development'),
    environment: optional(env, 'RAILWAY_ENVIRONMENT_NAME', 'local'),
    operatorSecret: operatorSecret || null,
    admissionPrivateKeyPem: admissionPrivateKeyPem || null,
    admissionKeyId: optional(env, 'ADMISSION_KEY_ID', 'latch-remote-link-v1'),
    admissionIssuer: optional(env, 'ADMISSION_ISSUER', 'https://control.latch.invalid'),
    relayUrl: optional(env, 'RELAY_URL', 'wss://relay.latch.invalid/v1/connect'),
    relayServiceToken: relayServiceToken || null,
    relayInvalidationSecret: relayInvalidationSecret || null,
    apns: apnsPartial === 3
      ? {
        keyId: apnsKeyId,
        teamId: apnsTeamId,
        privateKeyPem: apnsPrivateKeyPem,
        topic: optional(env, 'APNS_TOPIC', 'dev.cooperativ.latch.mobile'),
        environment: apnsEnvironment,
      }
      : null,
    attentionRatePerHost: integer(env, 'ATTENTION_RATE_PER_HOST', 60, 4, 1_000),
  };
}
