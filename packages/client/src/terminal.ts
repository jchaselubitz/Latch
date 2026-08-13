import { backoffDelay } from './reconnect';
import type { RetryPolicy, TerminalHandle, TerminalState } from './types';

const WRITE_QUEUE_LIMIT = 32;

type AttachTerminalOptions = {
  baseUrl: string;
  token: string;
  sessionId: string;
  retry: RetryPolicy;
  webSocket?: new (url: string, protocols?: string | string[]) => WebSocket;
};

export function attachTerminal(options: AttachTerminalOptions): TerminalHandle {
  const WebSocketImpl = options.webSocket ?? globalThis.WebSocket;
  if (!WebSocketImpl) {
    throw new Error('WebSocket is not available; pass webSocket to createLatchClient');
  }

  let closed = false;
  let socket: WebSocket | null = null;
  let attempt = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let state: TerminalState = 'connecting';
  let lastSize: { cols: number; rows: number } | null = null;
  const writeQueue: Uint8Array[] = [];
  const dataHandlers = new Set<(bytes: Uint8Array) => void>();
  const stateHandlers = new Set<(next: TerminalState) => void>();

  function setState(next: TerminalState) {
    if (state === next) {
      return;
    }
    state = next;
    for (const handler of stateHandlers) {
      handler(next);
    }
  }

  function socketUrl() {
    const url = new URL(options.baseUrl);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    url.pathname = `/v1/sessions/${encodeURIComponent(options.sessionId)}/terminal`;
    url.search = '';
    url.hash = '';
    return url.toString();
  }

  function flushQueue(target: WebSocket) {
    while (writeQueue.length > 0 && target.readyState === WebSocketImpl.OPEN) {
      const bytes = writeQueue.shift();
      if (bytes) {
        target.send(bytes);
      }
    }
  }

  function connect() {
    if (closed) {
      return;
    }
    const next = new WebSocketImpl(socketUrl(), [`latch.v1.${options.token}`]);
    next.binaryType = 'arraybuffer';
    socket = next;
    setState(attempt === 0 ? 'connecting' : 'reconnecting');

    next.onopen = () => {
      attempt = 0;
      setState('open');
      if (lastSize) {
        next.send(JSON.stringify({ type: 'resize', cols: lastSize.cols, rows: lastSize.rows }));
      }
      flushQueue(next);
    };

    next.onmessage = event => {
      if (typeof event.data === 'string') {
        return;
      }
      const bytes =
        event.data instanceof ArrayBuffer
          ? new Uint8Array(event.data)
          : event.data instanceof Uint8Array
            ? event.data
            : new Uint8Array(0);
      if (bytes.byteLength === 0) {
        return;
      }
      for (const handler of dataHandlers) {
        handler(bytes);
      }
    };

    next.onclose = () => {
      if (socket === next) {
        socket = null;
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
    write(bytes) {
      if (closed || bytes.byteLength === 0) {
        return;
      }
      if (socket && socket.readyState === WebSocketImpl.OPEN) {
        socket.send(bytes);
        return;
      }
      if (writeQueue.length >= WRITE_QUEUE_LIMIT) {
        writeQueue.shift();
      }
      writeQueue.push(bytes);
    },
    resize(size) {
      lastSize = size;
      if (socket && socket.readyState === WebSocketImpl.OPEN) {
        socket.send(JSON.stringify({ type: 'resize', cols: size.cols, rows: size.rows }));
      }
    },
    onData(handler) {
      dataHandlers.add(handler);
      return () => {
        dataHandlers.delete(handler);
      };
    },
    onState(handler) {
      stateHandlers.add(handler);
      handler(state);
      return () => {
        stateHandlers.delete(handler);
      };
    },
    close() {
      closed = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      socket?.close();
      socket = null;
      setState('closed');
    }
  };
}
