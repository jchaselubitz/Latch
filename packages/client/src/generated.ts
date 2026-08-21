// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 9748aeed7a8177c6a99df66613bec8a1bedc75dfb6d6f05a2adf7c4f37da4d77


export type TerminalAccessMode = 'control' | 'read-only';
export type GatewayFeatures = { readOnlyTerminal: boolean };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
