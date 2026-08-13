import type { RetryPolicy } from './types';

export const defaultRetryPolicy: RetryPolicy = {
  initialMs: 200,
  maxMs: 10_000,
  multiplier: 2
};

export function backoffDelay({
  attempt,
  policy
}: {
  attempt: number;
  policy: RetryPolicy;
}): number {
  if (attempt <= 0) {
    return 0;
  }
  return Math.min(policy.maxMs, policy.initialMs * policy.multiplier ** (attempt - 1));
}
