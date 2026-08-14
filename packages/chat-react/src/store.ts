import type { HarnessEvent } from '@latch/harness-schema';

export type TranscriptItem =
  | {
      id: string;
      kind: 'user';
      at: string;
      text: string;
    }
  | {
      id: string;
      kind: 'assistant';
      at: string;
      text: string;
      streaming: boolean;
    }
  | {
      id: string;
      kind: 'tool';
      at: string;
      tool: string;
      input: unknown;
      output?: unknown;
      finished: boolean;
    }
  | {
      id: string;
      kind: 'awaiting_input';
      at: string;
      requestId: string;
      requestKind: 'permission' | 'question';
      prompt: string;
      choices?: string[];
      // False only while this is the newest event in the stream. The
      // connector emits `awaiting_input` when the harness blocks, and has no
      // "resolved" event to pair with it — anything newer means the harness
      // moved on, so the prompt is history and must stop rendering as live.
      resolved: boolean;
    };

export type TranscriptState = {
  items: TranscriptItem[];
  status: string | null;
  cursor: number;
  connectorEpoch: number | null;
  harnessVersion: string | null;
  pendingRequestId: string | null;
};

export function emptyTranscript(): TranscriptState {
  return {
    items: [],
    status: null,
    cursor: 0,
    connectorEpoch: null,
    harnessVersion: null,
    pendingRequestId: null
  };
}

export function applyEvent({
  state,
  event
}: {
  state: TranscriptState;
  event: HarnessEvent;
}): TranscriptState {
  if (state.connectorEpoch != null && event.connectorEpoch !== state.connectorEpoch) {
    state = emptyTranscript();
  }
  state = resolvePending({ state });

  const cursor = state.cursor + 1;
  const base = {
    cursor,
    connectorEpoch: event.connectorEpoch,
    harnessVersion: event.harnessVersion,
    status: state.status,
    pendingRequestId: state.pendingRequestId
  };

  switch (event.type) {
    case 'user_message':
      return {
        ...base,
        items: [
          ...state.items,
          { id: itemId({ cursor, kind: 'user' }), kind: 'user', at: event.at, text: event.text }
        ]
      };
    case 'assistant_delta':
      return {
        ...base,
        items: accumulateAssistant({ items: state.items, at: event.at, text: event.text, cursor })
      };
    case 'assistant_message':
      return {
        ...base,
        items: completeAssistant({ items: state.items, at: event.at, text: event.text, cursor })
      };
    case 'tool_started':
      return {
        ...base,
        items: [
          ...state.items,
          {
            id: itemId({ cursor, kind: 'tool' }),
            kind: 'tool',
            at: event.at,
            tool: event.tool,
            input: event.input,
            finished: false
          }
        ]
      };
    case 'tool_finished':
      return {
        ...base,
        items: finishTool({ items: state.items, tool: event.tool, output: event.output })
      };
    case 'awaiting_input':
      return {
        ...base,
        pendingRequestId: event.requestId,
        items: [
          ...state.items,
          {
            id: itemId({ cursor, kind: 'awaiting_input' }),
            kind: 'awaiting_input',
            at: event.at,
            requestId: event.requestId,
            requestKind: event.kind,
            prompt: event.prompt,
            choices: event.choices,
            resolved: false
          }
        ]
      };
    case 'status':
      return {
        ...base,
        items: state.items,
        status: event.status
      };
  }
}

export function foldEvents({ events }: { events: HarnessEvent[] }): TranscriptState {
  return events.reduce(
    (state, event) => applyEvent({ state, event }),
    emptyTranscript()
  );
}

// Retires the open request before the next event is folded in.
//
// `pendingRequestId` is what binds `<AwaitingInputPrompt>` to the request it
// was rendered for; leaving a stale id there would let a widget stay live
// against a prompt the harness has already left behind.
function resolvePending({ state }: { state: TranscriptState }): TranscriptState {
  if (state.pendingRequestId == null) {
    return state;
  }
  const pending = state.pendingRequestId;
  const items = state.items.map(item =>
    item.kind === 'awaiting_input' && item.requestId === pending && !item.resolved
      ? { ...item, resolved: true }
      : item
  );
  return { ...state, items, pendingRequestId: null };
}

function itemId({ cursor, kind }: { cursor: number; kind: string }): string {
  return `${kind}:${cursor}`;
}

function accumulateAssistant({
  items,
  at,
  text,
  cursor
}: {
  items: TranscriptItem[];
  at: string;
  text: string;
  cursor: number;
}): TranscriptItem[] {
  const last = items.at(-1);
  if (last?.kind === 'assistant' && last.streaming) {
    return [...items.slice(0, -1), { ...last, text: last.text + text }];
  }
  return [
    ...items,
    {
      id: itemId({ cursor, kind: 'assistant' }),
      kind: 'assistant',
      at,
      text,
      streaming: true
    }
  ];
}

function completeAssistant({
  items,
  at,
  text,
  cursor
}: {
  items: TranscriptItem[];
  at: string;
  text: string;
  cursor: number;
}): TranscriptItem[] {
  const last = items.at(-1);
  if (last?.kind === 'assistant' && last.streaming) {
    return [...items.slice(0, -1), { ...last, text, streaming: false, at }];
  }
  return [
    ...items,
    {
      id: itemId({ cursor, kind: 'assistant' }),
      kind: 'assistant',
      at,
      text,
      streaming: false
    }
  ];
}

function finishTool({
  items,
  tool,
  output
}: {
  items: TranscriptItem[];
  tool: string;
  output: unknown;
}): TranscriptItem[] {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item?.kind === 'tool' && item.tool === tool && !item.finished) {
      const next = items.slice();
      next[index] = { ...item, output, finished: true };
      return next;
    }
  }
  return items;
}
