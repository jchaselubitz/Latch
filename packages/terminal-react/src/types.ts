import type { LatchClient } from '@latch/client';

export type TerminalSize = {
  cols: number;
  rows: number;
};

export type LatchTerminalRenderer = {
  write(bytes: Uint8Array): void;
  focus(): void;
  dispose(): void;
  onResize(handler: (size: TerminalSize) => void): () => void;
};

export type CreateTerminalRenderer = (options: { element: HTMLElement }) => LatchTerminalRenderer;

export type LatchTerminalProps = {
  client: LatchClient;
  sessionId: string;
  createRenderer?: CreateTerminalRenderer;
};
