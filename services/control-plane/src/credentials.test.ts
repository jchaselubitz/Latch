import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  bearerToken,
  digestOf,
  digestsMatch,
  issueCredential,
  newId,
  newSecret,
  subjectOf,
} from './credentials.ts';
import { RateLimiter } from './rate-limit.ts';

describe('credentials', () => {
  it('issues unique, subject-bound tokens whose digest is not the token', () => {
    const subject = newId('dev');
    const first = issueCredential('device', subject);
    const second = issueCredential('device', subject);
    assert.notEqual(first.token, second.token);
    assert.equal(subjectOf(first.token), subject);
    assert.notEqual(first.digest, first.token);
    assert.equal(first.digest, digestOf('device', first.token));
  });

  it('separates digest domains so a digest cannot cross credential kinds', () => {
    const token = `${newId('dev')}.${newSecret()}`;
    assert.notEqual(digestOf('device', token), digestOf('account', token));
    assert.notEqual(digestOf('device', token), digestOf('relay-ticket', token));
  });

  it('rejects malformed tokens and headers', () => {
    assert.equal(subjectOf('nodot'), null);
    assert.equal(subjectOf('.leading'), null);
    assert.equal(subjectOf('trailing.'), null);
    assert.equal(bearerToken(undefined), null);
    assert.equal(bearerToken('Basic abc'), null);
    assert.equal(bearerToken('Bearer has space'), null);
    assert.equal(bearerToken('Bearer dev_1.abc'), 'dev_1.abc');
  });

  it('compares digests without leaking length mismatches as exceptions', () => {
    assert.equal(digestsMatch('abc', 'abc'), true);
    assert.equal(digestsMatch('abc', 'abcd'), false);
  });
});

describe('rate limiter', () => {
  it('allows a budget per window and resets afterwards', () => {
    let clock = 0;
    const limiter = new RateLimiter(3, 1_000, () => clock);
    assert.deepEqual(
      [limiter.allow('a'), limiter.allow('a'), limiter.allow('a'), limiter.allow('a')],
      [true, true, true, false],
    );
    assert.equal(limiter.allow('b'), true);
    clock += 1_000;
    assert.equal(limiter.allow('a'), true);
  });
});
