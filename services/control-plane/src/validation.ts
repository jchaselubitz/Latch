/**
 * Strict allowlist-based request validation. Unknown properties are rejected
 * so terminal content and gateway credentials cannot be smuggled into control
 * plane storage through otherwise valid requests.
 */

import type { Permission } from './domain.ts';
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
const SAFE_LABEL = /^[\p{L}\p{N} ._'()-]{1,64}$/u;

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

function optionalString(
  body: Record<string, unknown>,
  field: string,
  pattern: RegExp,
  fallback: string,
): string {
  return body[field] === undefined ? fallback : requiredString(body, field, pattern);
}

export function opaqueId(body: Record<string, unknown>, field: string): string {
  return requiredString(body, field, OPAQUE_ID);
}

export function isOpaqueId(value: string): boolean {
  return OPAQUE_ID.test(value);
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

export function permission(
  body: Record<string, unknown>,
  field: string,
  fallback: Permission,
): Permission {
  const value = body[field];
  if (value === undefined) return fallback;
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
