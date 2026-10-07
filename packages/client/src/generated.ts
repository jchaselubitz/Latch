// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: 9ed43d89589f8b43f9f28633b8edd7fe9aef38757b24711c68bf6e26ded71944


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
 * that as shells only. `attachmentMaxBytes` is present exactly when the
 * gateway serves the attachments route. */
export type GatewayFeatures = {
  exclusiveTerminal: boolean;
  sessionAgents?: SessionAgent[];
  attachmentMaxBytes?: number;
};
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
  attachments?: boolean;
};
export type GatewayEndpointName = 'sessions' | 'preview' | 'terminal' | 'conversation' | 'browseDirectories' | 'createSession' | 'stopSession' | 'attachments';

export type ConversationItem = { id: string; ordinal: number; createdAt: string; kind: { type: "message"; role: "user" | "assistant"; text: string; status: "submitted" | "queued" | "observed" | "partial" | "complete" | "failed"; } | { type: "tool"; name: string; summary: string; status: "running" | "succeeded" | "failed"; parentMessageId?: string | null; } | { type: "request"; requestId: string; requestType: "permission" | "question"; prompt: string; choices: Array<string>; questions?: Array<{ question: string; header?: string; multiSelect: boolean; options: Array<{ label: string; description: string; }>; }>; status: "pending" | "resolved" | "dismissed"; }; };
export type ConversationState = { phase: "starting" | "idle" | "working" | "awaiting_input" | "exited" | "unavailable"; sendMessage: { enabled: boolean; reason?: string | null; }; resolveRequest: { enabled: boolean; reason?: string | null; }; cancelTurn?: { enabled: boolean; reason?: string | null; }; turnOutcome?: "answer" | "aborted" | "refusal" | "error" | null; commands?: Array<{ name: string; description: string; source?: string; }>; pendingRequest: string | null; connector: null | { id: string; version: string; }; };
export type ConversationServerMessage =
  | { type: "snapshot"; generation: string; revision: number; operationEpoch: string; items: Array<ConversationItem>; state: ConversationState; hasMoreBefore: boolean; reason?: "initial" | "generation" | "operation_epoch" | "overflow"; }
  | { type: "items_upserted"; generation: string; revision: number; items: Array<ConversationItem>; }
  | { type: "items_removed"; generation: string; revision: number; itemIds: Array<string>; }
  | { type: "state_changed"; generation: string; revision: number; state: ConversationState; }
  | { type: "operation_result"; operationId: string; status: "accepted" | "queued" | "refused" | "ambiguous" | "unknown"; itemId?: string | null; reason?: string | null; }
  | { type: "history_page"; requestId: string; items: Array<ConversationItem>; hasMoreBefore: boolean; }
  | { type: "error"; code: string; message: string; };
export type ConversationClientMessage =
  | { type: "resume"; generation?: string | null; afterRevision?: number | null; }
  | { type: "send_message"; operationEpoch: string; operationId: string; text: string; }
  | { type: "resolve_request"; operationEpoch: string; operationId: string; requestId: string; choice: string; answers?: never; } | { type: "resolve_request"; operationEpoch: string; operationId: string; requestId: string; answers: Record<string, string>; choice?: never; }
  | { type: "cancel_turn"; operationEpoch: string; operationId: string; }
  | { type: "operation_status"; operationId: string; }
  | { type: "history_request"; requestId: string; beforeOrdinal: number; limit: number; };
