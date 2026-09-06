// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 1959ddbd129ee68dd25683c44cafde0bd935016be357eee163603cb7b66eddb5


export type TerminalCloseReason =
  | 'detached'
  | 'stolen'
  | 'slow_client'
  | 'session_exited'
  | 'kernel_error';
export const TERMINAL_CLOSE_CODES = {
  detached: 1000,
  slow_client: 4408,
  stolen: 4409,
  session_exited: 4410,
  kernel_error: 4500
} as const satisfies Record<TerminalCloseReason, number>;
export type GatewayFeatures = { exclusiveTerminal: boolean };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
