/**
 * Request-body validation.
 *
 * Validation is strict and allowlist-based: an unknown property is a 400
 * rather than an ignored field. That is a privacy control as much as a
 * hygiene one — it makes it impossible for a client to smuggle terminal
 * output, a session name, or a gateway token into a control-plane row by
 * attaching an extra key to an otherwise valid request.
 */

import type { Candidate, Permission } from './domain.ts';
import { PERMISSIONS } from './domain.ts';

export class ValidationError extends Error {
  readonly field: string;

  constructor(field: string, message: string) {
    super(message);
    this.field = field;
  }
}

const OPAQUE_ID = /^[a-z]+_[0-9a-f]{32}$/;
const HEX_KEY = /^[0-9a-f]{64}$/;
const REQUEST_ID = /^[A-Za-z0-9._:-]{8,128}$/;
const RELAY_ID = /^[0-9a-f]{32}$/;
const SAFE_LABEL = /^[\p{L}\p{N} ._'()-]{1,64}$/u;
/** `host:port`, where host is an IPv4/IPv6 literal. Never a hostname. */
const CANDIDATE_ADDRESS = /^(?:\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-fA-F:.]{2,45}\]):\d{1,5}$/;

export function object(value: unknown, allowed: readonly string[]): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new ValidationError('body', 'body must be a JSON object');
  }
  const record = value as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (!allowed.includes(key)) {
      throw new ValidationError(key, `unexpected property "${key}"`);
    }
  }
  return record;
}

export function requiredString(
  body: Record<string, unknown>,
  field: string,
  pattern: RegExp,
): string {
  const value = body[field];
  if (typeof value !== 'string' || !pattern.test(value)) {
    throw new ValidationError(field, `${field} is missing or malformed`);
  }
  return value;
}

export function optionalString(
  body: Record<string, unknown>,
  field: string,
  pattern: RegExp,
  fallback: string,
): string {
  if (body[field] === undefined) {
    return fallback;
  }
  return requiredString(body, field, pattern);
}

export function opaqueId(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, OPAQUE_ID);
}

export function isOpaqueId(value: string): boolean {
  return OPAQUE_ID.test(value);
}

export function isRelayId(value: string): boolean {
  return RELAY_ID.test(value);
}

export function label(body: Record<string, unknown>, field: string, fallback: string): string {
  return optionalString(body, field, SAFE_LABEL, fallback);
}

export function requiredLabel(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, SAFE_LABEL);
}

export function publicKey(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, HEX_KEY);
}

export function requestId(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, REQUEST_ID);
}

export function relayId(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, RELAY_ID);
}

export function permission(
  body: Record<string, unknown>,
  field: string,
  fallback: Permission,
): Permission {
  const value = body[field];
  if (value === undefined) {
    return fallback;
  }
  if (typeof value !== 'string' || !PERMISSIONS.includes(value as Permission)) {
    throw new ValidationError(field, `${field} must be one of ${PERMISSIONS.join(', ')}`);
  }
  return value as Permission;
}

export function boolean(body: Record<string, unknown>, field: string): boolean {
  const value = body[field];
  if (typeof value !== 'boolean') {
    throw new ValidationError(field, `${field} must be a boolean`);
  }
  return value;
}

export interface CandidateRules {
  readonly max: number;
  readonly now: number;
  readonly maxLifetimeSeconds: number;
}

/**
 * Validates connection candidates. A candidate is an IP literal and port with
 * a bounded absolute expiry: no hostname (which could name an internal
 * service), no scheme, and no lifetime beyond the presence window.
 */
export function candidates(
  body: Record<string, unknown>,
  field: string,
  rules: CandidateRules,
): Candidate[] {
  const value = body[field];
  if (!Array.isArray(value) || value.length === 0) {
    throw new ValidationError(field, `${field} must be a non-empty array`);
  }
  if (value.length > rules.max) {
    throw new ValidationError(field, `${field} accepts at most ${rules.max} entries`);
  }
  return value.map((entry, index) => {
    const candidate = object(entry, ['address', 'expiresAt']);
    const address = requiredString(candidate, 'address', CANDIDATE_ADDRESS);
    const port = Number(address.slice(address.lastIndexOf(':') + 1));
    if (!Number.isInteger(port) || port < 1 || port > 65_535) {
      throw new ValidationError(`${field}[${index}].address`, 'candidate port is out of range');
    }
    const expiresAt = candidate.expiresAt;
    if (typeof expiresAt !== 'number' || !Number.isInteger(expiresAt)) {
      throw new ValidationError(`${field}[${index}].expiresAt`, 'expiresAt must be a unix second');
    }
    if (expiresAt <= rules.now || expiresAt > rules.now + rules.maxLifetimeSeconds) {
      throw new ValidationError(
        `${field}[${index}].expiresAt`,
        `expiresAt must be within ${rules.maxLifetimeSeconds} seconds`,
      );
    }
    return { address, expiresAt };
  });
}

export function expiresAt(
  body: Record<string, unknown>,
  field: string,
  now: number,
  maxLifetimeSeconds: number,
): number {
  const value = body[field];
  if (value === undefined) {
    return now + maxLifetimeSeconds;
  }
  if (typeof value !== 'number' || !Number.isInteger(value)) {
    throw new ValidationError(field, `${field} must be a unix second`);
  }
  if (value <= now || value > now + maxLifetimeSeconds) {
    throw new ValidationError(field, `${field} must be within ${maxLifetimeSeconds} seconds`);
  }
  return value;
}
