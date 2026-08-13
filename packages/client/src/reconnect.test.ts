import assert from 'node:assert/strict';
import test from 'node:test';

import { backoffDelay, defaultRetryPolicy } from './reconnect.ts';

test('backoff doubles until the ceiling', () => {
  assert.equal(backoffDelay({ attempt: 0, policy: defaultRetryPolicy }), 0);
  assert.equal(backoffDelay({ attempt: 1, policy: defaultRetryPolicy }), 200);
  assert.equal(backoffDelay({ attempt: 2, policy: defaultRetryPolicy }), 400);
  assert.equal(backoffDelay({ attempt: 3, policy: defaultRetryPolicy }), 800);
  assert.equal(
    backoffDelay({
      attempt: 20,
      policy: defaultRetryPolicy
    }),
    10_000
  );
});
