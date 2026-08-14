import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import type { CreateTerminalRenderer, LatchTerminalRenderer } from './types.ts';

const encoder = new TextEncoder();

export const createXtermRenderer: CreateTerminalRenderer = ({ element }) => {
  const term = new Terminal({
    convertEol: false,
    cursorBlink: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
    theme: {
      background: '#111111',
      foreground: '#e6e6e6'
    }
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(element);
  fit.fit();

  const observer = new ResizeObserver(() => {
    fit.fit();
  });
  observer.observe(element);

  const renderer: LatchTerminalRenderer = {
    write(bytes) {
      term.write(bytes);
    },
    focus() {
      term.focus();
    },
    dispose() {
      observer.disconnect();
      term.dispose();
    },
    onResize(handler) {
      const disposable = term.onResize(({ cols, rows }) => {
        handler({ cols, rows });
      });
      handler({ cols: term.cols, rows: term.rows });
      return () => disposable.dispose();
    },
    onInput(handler) {
      // `onData` gives UTF-16 text; `onBinary` gives a binary string of raw
      // bytes, which is what arrives for input the terminal could not express
      // as text. Both go to the PTY as bytes.
      const data = term.onData(text => {
        handler(encoder.encode(text));
      });
      const binary = term.onBinary(text => {
        const bytes = new Uint8Array(text.length);
        for (let index = 0; index < text.length; index += 1) {
          bytes[index] = text.charCodeAt(index) & 0xff;
        }
        handler(bytes);
      });
      return () => {
        data.dispose();
        binary.dispose();
      };
    }
  };
  return renderer;
};
