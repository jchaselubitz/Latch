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
import { isIP } from 'node:net';

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
const CANDIDATE_ADDRESS = /^(?<host>\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-fA-F:.]{2,45}\]):(?<port>\d{1,5})$/;
const ICE_UFRAG = /^[A-Za-z0-9+/_=-]{4,256}$/;
const ICE_PWD = /^[A-Za-z0-9+/_=-]{22,256}$/;
const ICE_FOUNDATION = /^[A-Za-z0-9+/_-]{1,32}$/;
const ICE_TYPES = ['host', 'srflx', 'prflx', 'relay'] as const;
const ICE_PROTOCOLS = ['udp', 'tcp'] as const;
const ICE_TCP_TYPES = ['active', 'passive', 'so'] as const;

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

export interface IceCredentials {
  readonly iceUfrag?: string;
  readonly icePwd?: string;
}

/** Validates the paired credentials ICE needs to authenticate connectivity checks. */
export function iceCredentials(body: Record<string, unknown>): IceCredentials {
  const hasUfrag = body.iceUfrag !== undefined;
  const hasPwd = body.icePwd !== undefined;
  if (hasUfrag !== hasPwd) {
    throw new ValidationError('iceUfrag', 'iceUfrag and icePwd must be supplied together');
  }
  if (!hasUfrag) return {};
  return {
    iceUfrag: requiredString(body, 'iceUfrag', ICE_UFRAG),
    icePwd: requiredString(body, 'icePwd', ICE_PWD),
  };
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
    const candidate = object(entry, [
      'address', 'expiresAt', 'type', 'priority', 'foundation', 'component', 'protocol',
      'relatedAddress', 'relatedPort', 'tcpType',
    ]);
    const address = endpoint(candidate, 'address', `${field}[${index}].address`);
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
    const metadata = candidateMetadata(candidate, `${field}[${index}]`);
    return { address, expiresAt, ...metadata };
  });
}

function endpoint(body: Record<string, unknown>, field: string, errorField: string): string {
  const address = requiredString(body, field, CANDIDATE_ADDRESS);
  const match = CANDIDATE_ADDRESS.exec(address);
  const host = match?.groups?.host;
  const port = Number(match?.groups?.port);
  if (!host || !Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new ValidationError(errorField, 'candidate port is out of range');
  }
  const literal = host.startsWith('[') ? host.slice(1, -1) : host;
  if (isIP(literal) === 0 || isLoopback(literal)) {
    throw new ValidationError(errorField, 'candidate address must be a non-loopback IP literal');
  }
  return address;
}

function candidateMetadata(body: Record<string, unknown>, prefix: string): Omit<Candidate, 'address' | 'expiresAt'> {
  const fields = ['type', 'priority', 'foundation', 'component', 'protocol'] as const;
  const hasMetadata = fields.some((field) => body[field] !== undefined);
  if (!hasMetadata) return {};
  if (!fields.every((field) => body[field] !== undefined)) {
    throw new ValidationError(prefix, 'ICE candidate metadata must include type, priority, foundation, component, and protocol');
  }
  const type = enumValue(body, 'type', ICE_TYPES, `${prefix}.type`);
  const priority = uint32(body, 'priority', `${prefix}.priority`);
  const foundation = requiredString(body, 'foundation', ICE_FOUNDATION);
  const component = body.component;
  if (component !== 1 && component !== 2) {
    throw new ValidationError(`${prefix}.component`, 'component must be 1 or 2');
  }
  const protocol = enumValue(body, 'protocol', ICE_PROTOCOLS, `${prefix}.protocol`);
  const relatedAddress = optionalIpLiteral(body, 'relatedAddress', `${prefix}.relatedAddress`);
  const relatedPort = optionalPort(body, 'relatedPort', `${prefix}.relatedPort`);
  if ((relatedAddress === undefined) !== (relatedPort === undefined)) {
    throw new ValidationError(prefix, 'relatedAddress and relatedPort must be supplied together');
  }
  const tcpType = body.tcpType === undefined
    ? undefined
    : enumValue(body, 'tcpType', ICE_TCP_TYPES, `${prefix}.tcpType`);
  if (tcpType !== undefined && protocol !== 'tcp') {
    throw new ValidationError(`${prefix}.tcpType`, 'tcpType is valid only for TCP candidates');
  }
  return { type, priority, foundation, component, protocol, relatedAddress, relatedPort, tcpType };
}

function enumValue<T extends readonly string[]>(
  body: Record<string, unknown>,
  field: string,
  values: T,
  errorField: string,
): T[number] {
  const value = body[field];
  if (typeof value !== 'string' || !values.includes(value)) {
    throw new ValidationError(errorField, `${field} is malformed`);
  }
  return value as T[number];
}

function uint32(body: Record<string, unknown>, field: string, errorField: string): number {
  const value = body[field];
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new ValidationError(errorField, `${field} must be an unsigned 32-bit integer`);
  }
  return value;
}

function optionalIpLiteral(
  body: Record<string, unknown>,
  field: string,
  errorField: string,
): string | undefined {
  const value = body[field];
  if (value === undefined) return undefined;
  if (typeof value !== 'string' || isIP(value) === 0 || isLoopback(value)) {
    throw new ValidationError(errorField, 'relatedAddress must be a non-loopback IP literal');
  }
  return value;
}

function optionalPort(body: Record<string, unknown>, field: string, errorField: string): number | undefined {
  const value = body[field];
  if (value === undefined) return undefined;
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 1 || value > 65_535) {
    throw new ValidationError(errorField, 'relatedPort must be between 1 and 65535');
  }
  return value;
}

function isLoopback(literal: string): boolean {
  if (literal.includes('.')) {
    const embedded = literal.match(/(\d{1,3}(?:\.\d{1,3}){3})$/);
    const ipv4 = embedded?.[1] ?? literal;
    return Number(ipv4.split('.')[0]) === 127;
  }
  const groups = literal.split('::');
  const expand = (part: string) => part ? part.split(':').filter(Boolean) : [];
  const left = expand(groups[0] ?? '');
  const right = expand(groups[1] ?? '');
  const zeros = Array(Math.max(0, 8 - left.length - right.length)).fill('0');
  const normalized = [...left, ...zeros, ...right].map((group) => Number.parseInt(group, 16));
  if (normalized.length !== 8) return false;
  if (normalized.slice(0, 7).every((group) => group === 0) && normalized[7] === 1) return true;
  // IPv4-mapped and deprecated IPv4-compatible IPv6 literals can encode
  // 127.0.0.0/8 without a dotted tail, e.g. ::ffff:7f00:1.
  const embeddedV4IsLoopback = (normalized[6]! >>> 8) === 127;
  const ipv4Compatible = normalized.slice(0, 6).every((group) => group === 0);
  const ipv4Mapped = normalized.slice(0, 5).every((group) => group === 0) && normalized[5] === 0xffff;
  return embeddedV4IsLoopback && (ipv4Compatible || ipv4Mapped);
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
