// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 6d8174fcbf2b24ec41f4eaca3e54eb3353e2b874b9c375fbe3665eb77f0eec74


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
export type GatewayFeatures = { exclusiveTerminal: boolean };
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
