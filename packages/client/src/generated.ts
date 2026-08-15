// Generated from schemas/remote-access/v1/*.schema.json; do not edit by hand.

export type TerminalAccessMode = 'control' | 'read-only';

export type GatewayFeatures = {
  idempotencyKeys: boolean;
  readOnlyTerminal: boolean;
};

export type GatewayReadiness = {
  formatVersion: 1;
  address: string;
  url: string;
  protocolVersion: 1;
  gatewayInstanceId: string;
};

export type IdempotencyKey = string;
