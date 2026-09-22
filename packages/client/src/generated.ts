// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: d5efabb3331ad7f5148b34aef3aacf571f30d7c3bd9345587bc550bcfa71448d


export type TerminalCloseReason =
  | 'detached'
  | 'stolen'
  | 'slow_client'
  | 'session_exited'
  | 'kernel_error'
  | 'resume_refused';
export const TERMINAL_CLOSE_CODES = {
  detached: 1000,
  slow_client: 4408,
  stolen: 4409,
  session_exited: 4410,
  kernel_error: 4500,
  resume_refused: 4411
} as const satisfies Record<TerminalCloseReason, number>;
export type SessionAgent = 'claude' | 'codex';
export type CreateSessionRequest = { requestId: string; cwd: string; agent?: SessionAgent };
/** `sessionAgents` is absent on a gateway that predates agent creation; read
 * that as shells only. */
export type GatewayFeatures = { exclusiveTerminal: boolean; sessionAgents?: SessionAgent[] };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
/** Routes the gateway serves. An absent key means the gateway predates
 * that route: treat it as unavailable, never as false-by-default. */
export type GatewayEndpoints = {
  sessions: boolean;
  preview?: boolean;
  terminal: boolean;
  conversation: boolean;
  browseDirectories?: boolean;
  createSession?: boolean;
  stopSession?: boolean;
};
export type GatewayEndpointName = 'sessions' | 'preview' | 'terminal' | 'conversation' | 'browseDirectories' | 'createSession' | 'stopSession';
