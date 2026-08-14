import type { GatewayCapabilities, GatewayEndpoint } from './types.ts';

// A gateway from before discovery existed is the step-1 terminal gateway.
// Its /v1 terminal and session paths predate optional events and send, so a
// newer client can still offer the useful fallback without probing endpoints
// that the gateway never promised.
export const legacyGatewayCapabilities = (): GatewayCapabilities => ({
  protocolVersion: 1,
  productVersion: 'unknown',
  capabilities: {
    create: false,
    openViewer: false,
    localAttach: true,
    cloudAttach: false,
    selfUpdate: false,
    extensions: []
  },
  endpoints: {
    sessions: true,
    sessionCapabilities: false,
    terminal: true,
    events: false,
    send: false
  }
});

// `/v1` is an additive protocol: an endpoint is usable only when discovery
// says it is present. A different major is intentionally not guessed at.
export function supportsGatewayEndpoint({
  capabilities,
  endpoint
}: {
  capabilities: GatewayCapabilities;
  endpoint: GatewayEndpoint;
}): boolean {
  return capabilities.protocolVersion === 1 && capabilities.endpoints[endpoint];
}
