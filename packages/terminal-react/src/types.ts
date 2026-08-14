import type { LatchClient, TerminalCloseInfo, TerminalState } from '@latch/client';

export type TerminalSize = {
  cols: number;
  rows: number;
};

export type LatchTerminalRenderer = {
  write(bytes: Uint8Array): void;
  focus(): void;
  dispose(): void;
  onResize(handler: (size: TerminalSize) => void): () => void;
  // Keystrokes and pasted bytes leaving the renderer, already UTF-8 encoded.
  // Without this the component is a viewer: the terminal socket is duplex and
  // `latch attach` is waiting on stdin.
  onInput(handler: (bytes: Uint8Array) => void): () => void;
};

export type CreateTerminalRenderer = (options: { element: HTMLElement }) => LatchTerminalRenderer;

export type LatchTerminalProps = {
  client: LatchClient;
  sessionId: string;
  createRenderer?: CreateTerminalRenderer;
  // Connection state, for embedders that want to show a reconnect banner.
  onState?: (state: TerminalState) => void;
  // Fires when the gateway closed the socket for good — a removed session,
  // say — so the embedder can stop showing a live terminal.
  onClose?: (info: TerminalCloseInfo) => void;
};
