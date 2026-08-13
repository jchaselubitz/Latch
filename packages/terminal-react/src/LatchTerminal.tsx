'use client';

import { useEffect, useRef } from 'react';

import type { LatchTerminalProps } from './types';
import { createXtermRenderer } from './xterm';

export function LatchTerminal({ client, sessionId, createRenderer }: LatchTerminalProps) {
  const elementRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const element = elementRef.current;
    if (!element) {
      return undefined;
    }
    const renderer = (createRenderer ?? createXtermRenderer)({ element });
    const terminal = client.attachTerminal({ sessionId });
    const stopData = terminal.onData(bytes => renderer.write(bytes));
    const stopResize = renderer.onResize(size => terminal.resize(size));
    renderer.focus();
    return () => {
      stopData();
      stopResize();
      terminal.close();
      renderer.dispose();
    };
  }, [client, sessionId, createRenderer]);

  return (
    <div
      ref={elementRef}
      data-latch-terminal=""
      style={{ width: '100%', height: '100%', minHeight: '12rem' }}
    />
  );
}
