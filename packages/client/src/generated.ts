// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 3d8030a162f35eba90359e42af84208a9159880cd81a9abce9dbd6b8920a1890


export type TerminalAccessMode = 'control' | 'read-only';
export type GatewayFeatures = { readOnlyTerminal: boolean };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
