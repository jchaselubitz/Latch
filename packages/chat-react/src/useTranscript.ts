import { useEffect, useState } from 'react';

import type { EventState, LatchClient, TerminalCloseInfo } from '@latch/client';

import { applyEvent, emptyTranscript, type TranscriptState } from './store.ts';
import {
  defaultTranscriptStorage,
  restoreTranscript,
  transcriptStorageKey,
  type TranscriptStorage
} from './persistence.ts';

export type UseTranscriptOptions = {
  client: LatchClient;
  sessionId: string;
  // Where the cursor and the folded timeline survive a reload. Omit for
  // `localStorage` when the host has it; pass `null` to keep the transcript in
  // memory only.
  storage?: TranscriptStorage | null;
  storageKey?: string;
  // Snapshots are written at most this often while events stream, and once
  // more when the subscription is torn down.
  saveIntervalMs?: number;
};

export type TranscriptView = TranscriptState & {
  connection: EventState;
  close: TerminalCloseInfo | null;
};

export function useTranscript({
  client,
  sessionId,
  storage,
  storageKey,
  saveIntervalMs = 250
}: UseTranscriptOptions): TranscriptView {
  const [view, setView] = useState<TranscriptView>(() => ({
    ...emptyTranscript(),
    connection: 'connecting',
    close: null
  }));

  useEffect(() => {
    const store = storage === undefined ? defaultTranscriptStorage() : storage;
    const key = storageKey ?? transcriptStorageKey({ sessionId });
    let state = restoreTranscript({ storage: store, key });
    let connection: EventState = 'connecting';
    let close: TerminalCloseInfo | null = null;
    let saveTimer: ReturnType<typeof setTimeout> | null = null;

    const flush = () => {
      if (saveTimer) {
        clearTimeout(saveTimer);
        saveTimer = null;
      }
      store?.save(key, state);
    };
    const scheduleSave = () => {
      if (!store || saveTimer) {
        return;
      }
      saveTimer = setTimeout(() => {
        saveTimer = null;
        store.save(key, state);
      }, saveIntervalMs);
    };
    const publish = () => {
      setView({ ...state, connection, close });
    };
    publish();

    // Resuming means asking the gateway for events after `cursor`, under the
    // epoch that minted it. A different epoch shifts every index, so the
    // subscription re-syncs from zero rather than splicing onto this timeline.
    const subscription = client.subscribeEvents({
      sessionId,
      cursor: state.cursor,
      ...(state.connectorEpoch != null ? { connectorEpoch: state.connectorEpoch } : {})
    });
    const stopState = subscription.onState(next => {
      connection = next;
      publish();
    });
    const stopClose = subscription.onClose(info => {
      close = info;
      publish();
    });
    const stopResync = subscription.onResync(() => {
      state = emptyTranscript();
      store?.clear(key);
      publish();
    });

    let stopped = false;
    void (async () => {
      for await (const event of subscription) {
        if (stopped) {
          return;
        }
        state = applyEvent({ state, event });
        scheduleSave();
        publish();
      }
    })();

    return () => {
      stopped = true;
      stopResync();
      stopClose();
      stopState();
      subscription.close();
      flush();
    };
  }, [client, sessionId, storage, storageKey, saveIntervalMs]);

  return view;
}
