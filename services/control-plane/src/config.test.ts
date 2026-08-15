import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { ConfigError, loadConfig } from './config.ts';

const base = { DATABASE_URL: 'postgres://localhost/latch' };

describe('configuration', () => {
  it('applies documented defaults', () => {
    const config = loadConfig(base);
    assert.equal(config.port, 8080);
    assert.equal(config.host, '0.0.0.0');
    assert.equal(config.presenceTtlSeconds, 90);
    assert.equal(config.relayTicketTtlSeconds, 60);
    assert.equal(config.maxDevicesPerAccount, 32);
    assert.equal(config.relayServiceToken, null);
    assert.equal(config.migrateOnBoot, true);
  });

  it('fails fast without a database url', () => {
    assert.throws(() => loadConfig({}), ConfigError);
  });

  it('rejects out-of-range and malformed values', () => {
    assert.throws(() => loadConfig({ ...base, PORT: 'http' }), ConfigError);
    assert.throws(() => loadConfig({ ...base, PRESENCE_TTL_SECONDS: '86400' }), ConfigError);
    assert.throws(() => loadConfig({ ...base, MIGRATE_ON_BOOT: 'maybe' }), ConfigError);
  });

  it('refuses a weak relay service token', () => {
    assert.throws(() => loadConfig({ ...base, RELAY_SERVICE_TOKEN: 'short' }), ConfigError);
    const config = loadConfig({ ...base, RELAY_SERVICE_TOKEN: 'x'.repeat(32) });
    assert.equal(config.relayServiceToken?.length, 32);
  });
});
