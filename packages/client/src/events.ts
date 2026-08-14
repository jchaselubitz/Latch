import type { HarnessEvent } from '@latch/harness-schema';

import { backoffDelay } from './reconnect.ts';
import type { EventSubscription, RetryPolicy, TerminalCloseInfo } from './types.ts';
import {
  FATAL_CLOSE_CODES,
  NO_CONNECTOR_CLOSE,
  SOCKET_OPEN,
  STALE_CURSOR_CLOSE,
  latchProtocols,
  sessionWsUrl
} from './ws.ts';

const EVENTS_FATAL_CLOSE_CODES = new Set([...FATAL_CLOSE_CODES, NO_CONNECTOR_CLOSE, 1000]);

type SubscribeEventsOptions = {
  baseUrl: string;
  token: string;
  sessionId: string;
  cursor?: number;
  connectorEpoch?: number;
  retry: RetryPolicy;
  webSocket?: new (url: string, protocols?: string | string[]) => WebSocket;
};

type Waiter = {
  resolve: () => void;
};

export function subscribeEvents(options: SubscribeEventsOptions): EventSubscription {
  const WebSocketImpl = options.webSocket ?? globalThis.WebSocket;
  if (!WebSocketImpl) {
    throw new Error('WebSocket is not available; pass webSocket to createLatchClient');
  }

  let closed = false;
  let socket: WebSocket | null = null;
  let attempt = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let nextCursor = options.cursor ?? 0;
  let epoch = options.connectorEpoch;
  let resyncedFromZero = nextCursor === 0;
  let resyncing = false;
  let streamState: 'connecting' | 'open' | 'reconnecting' | 'closed' = 'connecting';
  const queue: HarnessEvent[] = [];
  let waiter: Waiter | null = null;
  const stateHandlers = new Set<(next: 'connecting' | 'open' | 'reconnecting' | 'closed') => void>();
  const closeHandlers = new Set<(info: TerminalCloseInfo) => void>();
  const resyncHandlers = new Set<() => void>();

  function wake() {
    waiter?.resolve();
    waiter = null;
  }

  function emitClose(info: TerminalCloseInfo) {
    for (const handler of closeHandlers) {
      handler(info);
    }
    wake();
  }

  function emitResync() {
    nextCursor = 0;
    epoch = undefined;
    resyncedFromZero = true;
    resyncing = true;
    for (const handler of resyncHandlers) {
      handler();
    }
  }

  function setState(next: 'connecting' | 'open' | 'reconnecting' | 'closed') {
    if (streamState === next) {
      return;
    }
    streamState = next;
    for (const handler of stateHandlers) {
      handler(next);
    }
  }

  function socketUrl() {
    return sessionWsUrl({
      baseUrl: options.baseUrl,
      sessionId: options.sessionId,
      channel: 'events',
      query: { cursor: String(nextCursor) }
    });
  }

  function connect() {
    if (closed) {
      return;
    }
    const next = new WebSocketImpl(socketUrl(), latchProtocols({ token: options.token }));
    socket = next;
    setState(attempt === 0 ? 'connecting' : 'reconnecting');

    next.onopen = () => {
      attempt = 0;
      setState('open');
    };

    next.onmessage = event => {
      if (typeof event.data !== 'string') {
        return;
      }
      const parsed = parseHarnessEvent(event.data);
      if (!parsed) {
        return;
      }
      if (epoch != null && parsed.connectorEpoch !== epoch && nextCursor > 0) {
        emitResync();
        next.close();
        return;
      }
      epoch = parsed.connectorEpoch;
      nextCursor += 1;
      resyncedFromZero = false;
      queue.push(parsed);
      wake();
    };

    next.onclose = event => {
      if (socket === next) {
        socket = null;
      }
      if (closed) {
        setState('closed');
        wake();
        return;
      }
      if (resyncing) {
        resyncing = false;
        scheduleReconnect();
        return;
      }
      const code = event?.code ?? 0;
      const reason = event?.reason ?? '';
      if (code === STALE_CURSOR_CLOSE && !resyncedFromZero) {
        emitResync();
        resyncing = false;
        scheduleReconnect();
        return;
      }
      if (EVENTS_FATAL_CLOSE_CODES.has(code)) {
        closed = true;
        emitClose({ code, reason });
        setState('closed');
        return;
      }
      scheduleReconnect();
    };

    next.onerror = () => {
      next.close();
    };
  }

  function scheduleReconnect() {
    if (closed) {
      setState('closed');
      wake();
      return;
    }
    setState('reconnecting');
    attempt += 1;
    const delay = backoffDelay({ attempt, policy: options.retry });
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, delay);
  }

  connect();

  return {
    sessionId: options.sessionId,
    get cursor() {
      return nextCursor;
    },
    async *[Symbol.asyncIterator]() {
      while (!closed || queue.length > 0) {
        if (queue.length === 0) {
          if (closed) {
            return;
          }
          await new Promise<void>(resolve => {
            waiter = { resolve };
          });
          continue;
        }
        const event = queue.shift();
        if (event) {
          yield event;
        }
      }
    },
    onState(handler) {
      stateHandlers.add(handler);
      handler(streamState);
      return () => {
        stateHandlers.delete(handler);
      };
    },
    onClose(handler) {
      closeHandlers.add(handler);
      return () => {
        closeHandlers.delete(handler);
      };
    },
    onResync(handler) {
      resyncHandlers.add(handler);
      return () => {
        resyncHandlers.delete(handler);
      };
    },
    close() {
      closed = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      if (socket && socket.readyState === SOCKET_OPEN) {
        socket.close();
      } else {
        socket?.close();
      }
      socket = null;
      setState('closed');
      wake();
    }
  };
}

function parseHarnessEvent(raw: string): HarnessEvent | null {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed === 'object' &&
      'type' in parsed &&
      typeof (parsed as { type: unknown }).type === 'string'
    ) {
      return parsed as HarnessEvent;
    }
  } catch {
    return null;
  }
  return null;
}
