import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { ConfigError, loadConfig } from './config.ts';

const base = { DATABASE_URL: 'postgres://localhost/latch' };

describe('configuration', () => {
  it('applies documented defaults', () => {
    const config = loadConfig(base);
    assert.equal(config.port, 8080);
    assert.equal(config.host, '0.0.0.0');
    assert.equal(config.maxDevicesPerAccount, 32);
    assert.equal(config.migrateOnBoot, true);
    assert.equal(config.admissionRatePerDevice, 60);
    assert.equal(config.admissionRatePerOwner, 180);
    assert.equal(config.admissionRatePerIp, 120);
    assert.equal(config.admissionRateGlobal, 1_000);
    assert.equal(config.trustProxy, false);
  });

  it('fails fast without a database url', () => {
    assert.throws(() => loadConfig({}), ConfigError);
  });

  it('rejects out-of-range and malformed values', () => {
    assert.throws(() => loadConfig({ ...base, PORT: 'http' }), ConfigError);
    assert.throws(() => loadConfig({ ...base, MIGRATE_ON_BOOT: 'maybe' }), ConfigError);
    assert.throws(() => loadConfig({ ...base, REMOTE_ADMISSION_RATE_PER_DEVICE: '2' }), ConfigError);
  });
});
