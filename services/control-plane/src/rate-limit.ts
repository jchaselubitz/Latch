/**
 * Per-device fixed-window rate limiting.
 *
 * The threat model calls for rejecting connection exhaustion before any work
 * is done and for recording only aggregate failure categories. This limiter is
 * per-process, which is sufficient because the limit exists to bound abuse per
 * replica; the relay enforces its own ticket, frame, and byte quotas.
 */

export class RateLimiter {
  readonly #limit: number;
  readonly #windowMs: number;
  readonly #now: () => number;
  #counters = new Map<string, { windowStartedAt: number; count: number }>();

  constructor(limit: number, windowMs: number, now: () => number) {
    this.#limit = limit;
    this.#windowMs = windowMs;
    this.#now = now;
  }

  /** Records one request for `key` and reports whether it is within budget. */
  allow(key: string): boolean {
    const now = this.#now();
    const counter = this.#counters.get(key);
    if (!counter || now - counter.windowStartedAt >= this.#windowMs) {
      this.#counters.set(key, { windowStartedAt: now, count: 1 });
      this.#sweep(now);
      return true;
    }
    counter.count += 1;
    return counter.count <= this.#limit;
  }

  /** Drops idle counters so a long-lived process does not accumulate keys. */
  #sweep(now: number): void {
    if (this.#counters.size < 4096) {
      return;
    }
    for (const [key, counter] of this.#counters) {
      if (now - counter.windowStartedAt >= this.#windowMs) {
        this.#counters.delete(key);
      }
    }
  }
}
