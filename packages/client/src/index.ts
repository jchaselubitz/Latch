export type {
  EventState,
  EventSubscription,
  EventsCapability,
  GatewayCapabilities,
  GatewayEndpoint,
  InspectReport,
  LatchClient,
  LatchClientOptions,
  ListReport,
  RetryPolicy,
  SendReport,
  SendRequest,
  SessionCapabilities,
  SessionSummary,
  TerminalCloseInfo,
  TerminalAccessMode,
  TerminalHandle,
  TerminalState
} from './types.ts';
// Re-exported so an embedder can name what `sessionCapabilities` returns
// without adding `@latch/harness-schema` as a direct dependency.
export type { HarnessEvent, InteractionCapabilities } from '@latch/harness-schema';
export { createLatchClient, LatchGatewayError, LatchSendError } from './client.ts';
export { legacyGatewayCapabilities, supportsGatewayEndpoint } from './compatibility.ts';
export { backoffDelay, defaultRetryPolicy } from './reconnect.ts';
export type { GatewayReadiness, IdempotencyKey } from './generated.ts';
