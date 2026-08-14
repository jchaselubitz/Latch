import { useCallback, useEffect, useState } from 'react';

import type { LatchClient, SessionCapabilities } from '@latch/client';

export type UseSessionCapabilitiesOptions = {
  client: LatchClient;
  sessionId: string;
  intervalMs?: number;
};

export type SessionCapabilitiesView = {
  capabilities: SessionCapabilities | null;
  error: string | null;
  refresh: () => Promise<void>;
};

export function useSessionCapabilities({
  client,
  sessionId,
  intervalMs = 1000
}: UseSessionCapabilitiesOptions): SessionCapabilitiesView {
  const [capabilities, setCapabilities] = useState<SessionCapabilities | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await client.canSend({ sessionId });
      setCapabilities(next);
      setError(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : 'capabilities failed');
    }
  }, [client, sessionId]);

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      if (stopped) {
        return;
      }
      await refresh();
    };
    void tick();
    const timer = setInterval(() => {
      void tick();
    }, intervalMs);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [refresh, intervalMs]);

  return { capabilities, error, refresh };
}
