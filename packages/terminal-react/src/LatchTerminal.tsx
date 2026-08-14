'use client';

import { useEffect, useRef } from 'react';

import type { TerminalHandle } from '@latch/client';

import type { LatchTerminalProps } from './types.ts';
import { createXtermRenderer } from './xterm.ts';

export function LatchTerminal({
  client,
  sessionId,
  createRenderer,
  onState,
  onClose
}: LatchTerminalProps) {
  const elementRef = useRef<HTMLDivElement | null>(null);
  // Read through refs so an inline callback prop does not tear down the PTY
  // and reattach on every render of the parent.
  const createRendererRef = useRef(createRenderer);
  createRendererRef.current = createRenderer;
  const onStateRef = useRef(onState);
  onStateRef.current = onState;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const element = elementRef.current;
    if (!element) {
      return undefined;
    }
    const renderer = (createRendererRef.current ?? createXtermRenderer)({ element });
    let terminal: TerminalHandle | undefined;
    let initialSize: { cols: number; rows: number } | undefined;
    const stopResize = renderer.onResize(size => {
      initialSize = size;
      terminal?.resize(size);
    });
    terminal = client.attachTerminal({
      sessionId,
      cols: initialSize?.cols,
      rows: initialSize?.rows
    });
    const attached = terminal;
    const stopInput = renderer.onInput(bytes => attached.write(bytes));
    const stopData = attached.onData(bytes => renderer.write(bytes));
    const stopState = attached.onState(state => onStateRef.current?.(state));
    const stopClose = attached.onClose(info => onCloseRef.current?.(info));
    renderer.focus();
    return () => {
      stopClose();
      stopState();
      stopData();
      stopInput();
      stopResize();
      attached.close();
      renderer.dispose();
    };
  }, [client, sessionId]);

  return (
    <div
      ref={elementRef}
      data-latch-terminal=""
      style={{ width: '100%', height: '100%', minHeight: '12rem' }}
    />
  );
}
