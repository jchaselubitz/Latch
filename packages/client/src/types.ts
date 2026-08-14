import type { HarnessEvent, InteractionCapabilities } from '@latch/harness-schema';

export type RetryPolicy = {
  initialMs: number;
  maxMs: number;
  multiplier: number;
};

export type LatchClientOptions = {
  url: string;
  token: string;
  fetch?: typeof fetch;
  webSocket?: new (url: string, protocols?: string | string[]) => WebSocket;
  retry?: RetryPolicy;
};

export type SessionSummary = {
  id: string;
  name: string;
  title?: string;
  state: string;
  cwd: string;
  command_label: string;
  created_at: string;
  last_activity_at?: string;
  idle_ms?: number;
};

export type ListReport = {
  sessions: SessionSummary[];
};

export type TerminalSize = {
  cols: number;
  rows: number;
};

export type InspectReport = {
  id: string;
  name: string;
  title?: string;
  state: string;
  cwd: string;
  command_label: string;
  created_at: string;
  initial_size: TerminalSize;
  size?: TerminalSize;
  exit?: {
    code?: number;
    signal?: string;
    exited_at: string;
  };
  attached?: number;
};

export type TerminalState = 'connecting' | 'open' | 'reconnecting' | 'closed';

// Why a terminal socket stopped for good.
export type TerminalCloseInfo = {
  // WebSocket close code. `latch serve` uses 4404 for a missing session.
  code: number;
  // Close reason as the gateway phrased it.
  reason: string;
};

export type TerminalHandle = {
  readonly sessionId: string;
  write(bytes: Uint8Array): void;
  resize(size: { cols: number; rows: number }): void;
  onData(handler: (bytes: Uint8Array) => void): () => void;
  onState(handler: (state: TerminalState) => void): () => void;
  // Fires once when the gateway closed the socket for a reason no retry
  // fixes; the handle is `closed` and will not reconnect.
  onClose(handler: (info: TerminalCloseInfo) => void): () => void;
  close(): void;
};

export type EventState = TerminalState;

export type GatewayCapabilities = {
  protocolVersion: number;
  productVersion: string;
  capabilities: {
    create: boolean;
    openViewer: boolean;
    localAttach: boolean;
    cloudAttach: boolean;
    selfUpdate: boolean;
    extensions: string[];
  };
  endpoints: {
    sessions: boolean;
    sessionCapabilities: boolean;
    terminal: boolean;
    events: boolean;
    send: boolean;
  };
};

export type GatewayEndpoint = keyof GatewayCapabilities['endpoints'];

export type EventsCapability = {
  ok: boolean;
  reason?: string;
  harness?: string;
  connectorEpoch?: number;
};

export type SessionCapabilities = InteractionCapabilities & {
  events: EventsCapability;
};

export type EventSubscription = {
  readonly sessionId: string;
  readonly cursor: number;
  [Symbol.asyncIterator](): AsyncIterator<HarnessEvent>;
  onState(handler: (state: EventState) => void): () => void;
  onClose(handler: (info: TerminalCloseInfo) => void): () => void;
  // Fires before the socket replays from cursor 0 after a stale cursor or
  // connector-epoch change, so a transcript store can drop the old timeline.
  onResync(handler: () => void): () => void;
  close(): void;
};

export type SendRequest = {
  sessionId: string;
} & (
  | { message: string }
  | { keys: string }
  | { resolve: { requestId: string; choice: string } }
);

export type SendReport = {
  sessionId: string;
  operation: 'message' | 'keys' | 'resolve';
  requestId?: string;
  choice?: string;
  sent: boolean;
  resolved: boolean;
};

export type LatchClient = {
  listSessions(): Promise<ListReport>;
  inspectSession(options: { sessionId: string }): Promise<InspectReport>;
  gatewayCapabilities(): Promise<GatewayCapabilities>;
  sessionCapabilities(options: { sessionId: string }): Promise<SessionCapabilities>;
  // Screen-derived UX preflight. Composer disables on `canSend.ok === false`.
  // This is racy; `send()` always POSTs and a 409 is the authority.
  canSend(options: { sessionId: string }): Promise<SessionCapabilities>;
  send(request: SendRequest): Promise<SendReport>;
  attachTerminal(options: {
    sessionId: string;
    cols?: number;
    rows?: number;
  }): TerminalHandle;
  subscribeEvents(options: {
    sessionId: string;
    cursor?: number;
    connectorEpoch?: number;
  }): EventSubscription;
};
