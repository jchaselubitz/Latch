import { emptyTranscript, type TranscriptState } from './store.ts';

// Snapshot envelope version. A stored snapshot written by an older shape is
// discarded rather than migrated: the transcript is derivable from the event
// stream, so re-syncing from cursor 0 always costs less than a migration bug.
export const SNAPSHOT_VERSION = 1;

// Where a transcript snapshot survives a page reload or an app restart.
export type TranscriptStorage = {
  load(key: string): TranscriptState | null;
  save(key: string, state: TranscriptState): void;
  clear(key: string): void;
};

// The three methods of `Storage` this uses. Typed structurally so a React
// Native `AsyncStorage`-style shim or a test double satisfies it without
// pulling in the DOM lib.
export type WebStorageLike = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
};

export function transcriptStorageKey({ sessionId }: { sessionId: string }): string {
  return `latch.transcript.v${SNAPSHOT_VERSION}.${sessionId}`;
}

export function memoryTranscriptStorage(): TranscriptStorage {
  const entries = new Map<string, string>();
  return {
    load(key) {
      return decodeSnapshot({ raw: entries.get(key) ?? null });
    },
    save(key, state) {
      entries.set(key, encodeSnapshot({ state }));
    },
    clear(key) {
      entries.delete(key);
    }
  };
}

export function webTranscriptStorage({ storage }: { storage: WebStorageLike }): TranscriptStorage {
  return {
    load(key) {
      let raw: string | null = null;
      try {
        raw = storage.getItem(key);
      } catch {
        return null;
      }
      const state = decodeSnapshot({ raw });
      if (!state && raw != null) {
        // A snapshot we cannot read is worse than none: drop it so the next
        // save is not blocked by a quota the garbage still occupies.
        try {
          storage.removeItem(key);
        } catch {
          // Storage that refuses removal also refuses writes; nothing to do.
        }
      }
      return state;
    },
    save(key, state) {
      try {
        storage.setItem(key, encodeSnapshot({ state }));
      } catch {
        // Private-mode and quota failures must not break the live stream.
      }
    },
    clear(key) {
      try {
        storage.removeItem(key);
      } catch {
        // As above.
      }
    }
  };
}

// `localStorage` when the host has it, otherwise no persistence.
//
// Returning null rather than a memory store is deliberate: a memory store
// would look like persistence and lose everything on reload.
export function defaultTranscriptStorage(): TranscriptStorage | null {
  const candidate = (globalThis as { localStorage?: WebStorageLike }).localStorage;
  if (!candidate) {
    return null;
  }
  return webTranscriptStorage({ storage: candidate });
}

// Reads a snapshot and rejects anything that cannot be resumed from.
//
// A restored state is only useful as a cursor if the epoch that minted the
// cursor is known — resuming at index N under a different connector epoch
// would splice a shifted stream onto the old timeline.
export function restoreTranscript({
  storage,
  key
}: {
  storage: TranscriptStorage | null;
  key: string;
}): TranscriptState {
  if (!storage) {
    return emptyTranscript();
  }
  const restored = storage.load(key);
  if (!restored) {
    return emptyTranscript();
  }
  if (restored.cursor > 0 && restored.connectorEpoch == null) {
    storage.clear(key);
    return emptyTranscript();
  }
  return restored;
}

export function encodeSnapshot({ state }: { state: TranscriptState }): string {
  return JSON.stringify({ version: SNAPSHOT_VERSION, state });
}

export function decodeSnapshot({ raw }: { raw: string | null }): TranscriptState | null {
  if (!raw) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== 'object') {
    return null;
  }
  const envelope = parsed as { version?: unknown; state?: unknown };
  if (envelope.version !== SNAPSHOT_VERSION) {
    return null;
  }
  return validState({ candidate: envelope.state });
}

function validState({ candidate }: { candidate: unknown }): TranscriptState | null {
  if (!candidate || typeof candidate !== 'object') {
    return null;
  }
  const state = candidate as Partial<TranscriptState>;
  if (!Array.isArray(state.items)) {
    return null;
  }
  if (typeof state.cursor !== 'number' || !Number.isInteger(state.cursor) || state.cursor < 0) {
    return null;
  }
  if (state.connectorEpoch != null && typeof state.connectorEpoch !== 'number') {
    return null;
  }
  if (state.harnessVersion != null && typeof state.harnessVersion !== 'string') {
    return null;
  }
  if (state.status != null && typeof state.status !== 'string') {
    return null;
  }
  if (state.pendingRequestId != null && typeof state.pendingRequestId !== 'string') {
    return null;
  }
  return {
    items: state.items,
    status: state.status ?? null,
    cursor: state.cursor,
    connectorEpoch: state.connectorEpoch ?? null,
    harnessVersion: state.harnessVersion ?? null,
    pendingRequestId: state.pendingRequestId ?? null
  };
}
