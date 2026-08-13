import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import type { CreateTerminalRenderer, LatchTerminalRenderer } from './types';

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
    }
  };
  return renderer;
};
