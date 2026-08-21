import type { GatewayFeatures, TerminalAccessMode } from './generated.ts';

export type {
  GatewayFeatures,
  TerminalAccessMode
} from './generated.ts';

export type RetryPolicy = { initialMs: number; maxMs: number; multiplier: number };

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

export type ListReport = { sessions: SessionSummary[] };
export type TerminalSize = { cols: number; rows: number };
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
  exit?: { code?: number; signal?: string; exited_at: string };
  attached?: number;
};

export type TerminalState = 'connecting' | 'open' | 'reconnecting' | 'closed';
export type TerminalCloseInfo = { code: number; reason: string };
export type TerminalHandle = {
  readonly sessionId: string;
  write(bytes: Uint8Array): void;
  resize(size: TerminalSize): void;
  onData(handler: (bytes: Uint8Array) => void): () => void;
  onState(handler: (state: TerminalState) => void): () => void;
  onClose(handler: (info: TerminalCloseInfo) => void): () => void;
  close(): void;
};

export type GatewayCapabilities = {
  protocolVersion: 2;
  productVersion: string;
  capabilities: {
    create: boolean;
    openViewer: boolean;
    localAttach: boolean;
    cloudAttach: boolean;
    selfUpdate: boolean;
    extensions: string[];
  };
  endpoints: { sessions: boolean; terminal: boolean; conversation: boolean };
  features: GatewayFeatures;
  gatewayInstanceId: string;
  operationRetentionSeconds: number;
};

export type LatchClient = {
  listSessions(): Promise<ListReport>;
  inspectSession(options: { sessionId: string }): Promise<InspectReport>;
  gatewayCapabilities(): Promise<GatewayCapabilities>;
  attachTerminal(options: {
    sessionId: string;
    cols?: number;
    rows?: number;
    mode?: TerminalAccessMode;
  }): TerminalHandle;
};
