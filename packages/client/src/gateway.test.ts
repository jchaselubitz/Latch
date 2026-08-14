// End-to-end cover for the wire contract: the real `latch serve`, driven by
// the real client over Node's WebSocket. Every other test in this package
// fakes the socket, so nothing else would notice if the gateway renamed a
// field, moved a path, or changed a close code.
//
// Skipped when `target/debug/latch` has not been built or python3 is missing —
// `just check` runs `cargo test` first, so the binary is there by the time
// `check-web` runs.
import assert from 'node:assert/strict';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import type { HarnessEvent } from '@latch/harness-schema';

import { createLatchClient, LatchSendError } from './client.ts';
import type { LatchClient } from './types.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const latchBin = join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), 'debug', 'latch');

function prerequisitesMissing(): string | null {
  try {
    readFileSync(latchBin);
  } catch {
    return `no latch binary at ${latchBin}; run cargo build first`;
  }
  if (spawnSync('python3', ['--version']).status !== 0) {
    return 'python3 is required by the fake tmux';
  }
  return null;
}

type Harness = {
  home: string;
  env: NodeJS.ProcessEnv;
  sessionId: string;
  cwd: string;
  setScreen(screen: string): void;
  dispose(): void;
};

function startHarness(): Harness {
  const temp = mkdtempSync(join(tmpdir(), 'latch-sdk-smoke-'));
  const home = join(temp, 'home');
  const tmux = join(temp, 'tmux-3.7b');
  copyFileSync(join(root, 'fixtures/testing/fake-tmux.py'), tmux);
  chmodSync(tmux, 0o755);
  const claudeConfig = join(temp, 'claude');

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    LATCH_HOME: home,
    LATCH_TMUX_BIN: tmux,
    LATCH_INHERITED: 'fresh-client-environment',
    CLAUDE_CONFIG_DIR: claudeConfig
  };
  delete env.LATCH_SESSION_ID;

  const manifest = {
    format_version: 1,
    launch: {
      argv: ['/bin/sh', '-c', 'sleep 30'],
      cwd: temp,
      env: {},
      inherit_env: true,
      size: { cols: 100, rows: 30 },
      term: 'xterm-256color'
    },
    display: {
      name: 'agent',
      title: 'SDK smoke',
      command_label: 'claude',
      source: { kind: 'test', external_run_id: 'run-1' }
    }
  };
  const created = spawnSync(latchBin, ['create', '--manifest-file', '-', '--json'], {
    env,
    input: JSON.stringify(manifest),
    encoding: 'utf8'
  });
  assert.equal(created.status, 0, created.stderr);
  const sessionId = (JSON.parse(created.stdout) as { session: { id: string } }).session.id;

  // The connector reads Claude Code's own transcript, so the fixture the Rust
  // connector is verified against is what the events socket must deliver.
  const encoded = temp.replace(/[^0-9A-Za-z]/g, '-');
  const project = join(claudeConfig, 'projects', encoded);
  mkdirSync(project, { recursive: true });
  copyFileSync(
    join(root, 'fixtures/harness/claude-code/conversation/raw.jsonl'),
    join(project, 'run-1.jsonl')
  );

  const patchState = (patch: (entry: Record<string, unknown>) => void) => {
    const path = join(home, 'server.fake.json');
    const state = JSON.parse(readFileSync(path, 'utf8')) as Record<string, Record<string, unknown>>;
    const entry = state[sessionId] ?? {};
    patch(entry);
    state[sessionId] = entry;
    writeFileSync(path, JSON.stringify(state));
  };

  return {
    home,
    env,
    sessionId,
    cwd: temp,
    setScreen(screen) {
      patchState(entry => {
        entry.screen = screen;
      });
    },
    dispose() {
      spawnSync(latchBin, ['stop', sessionId, '--json'], { env });
      rmSync(temp, { recursive: true, force: true });
    }
  };
}

type Gateway = {
  url: string;
  token: string;
  stop(): void;
};

async function startGateway({ harness }: { harness: Harness }): Promise<Gateway> {
  const minted = spawnSync(latchBin, ['serve', 'token'], { env: harness.env, encoding: 'utf8' });
  assert.equal(minted.status, 0, minted.stderr);
  const token = minted.stdout.trim();

  const child: ChildProcess = spawn(latchBin, ['serve', '--bind', '127.0.0.1:0'], {
    env: { ...harness.env, LATCH_FAKE_TMUX_ECHO: '1' },
    stdio: ['ignore', 'ignore', 'pipe']
  });
  const address = await new Promise<string>((resolveAddress, rejectAddress) => {
    let buffered = '';
    const timer = setTimeout(() => {
      rejectAddress(new Error(`latch serve did not bind; stderr: ${buffered}`));
    }, 10_000);
    child.stderr?.on('data', (chunk: Buffer) => {
      buffered += chunk.toString();
      const match = /latch serve listening on (\S+)/.exec(buffered);
      if (match?.[1]) {
        clearTimeout(timer);
        resolveAddress(match[1]);
      }
    });
    child.on('exit', code => {
      clearTimeout(timer);
      rejectAddress(new Error(`latch serve exited with ${code}; stderr: ${buffered}`));
    });
  });

  return {
    url: `http://${address}`,
    token,
    stop() {
      child.kill('SIGKILL');
    }
  };
}

async function collectEvents({
  client,
  sessionId,
  cursor,
  count
}: {
  client: LatchClient;
  sessionId: string;
  cursor: number;
  count: number;
}): Promise<{ events: HarnessEvent[]; resyncs: number }> {
  const subscription = client.subscribeEvents({ sessionId, cursor });
  let resyncs = 0;
  subscription.onResync(() => {
    resyncs += 1;
  });
  const events: HarnessEvent[] = [];
  const timer = setTimeout(() => {
    subscription.close();
  }, 15_000);
  try {
    for await (const event of subscription) {
      events.push(event);
      if (events.length >= count) {
        break;
      }
    }
  } finally {
    clearTimeout(timer);
    subscription.close();
  }
  return { events, resyncs };
}

// A rejected race leaves its timer armed, which holds the event loop open
// long after the assertion has already passed or failed.
async function withDeadline({
  work,
  ms,
  message
}: {
  work: Promise<void>;
  ms: number;
  message: () => string;
}): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const deadline = new Promise<void>((_, rejectDeadline) => {
    timer = setTimeout(() => rejectDeadline(new Error(message())), ms);
  });
  try {
    await Promise.race([work, deadline]);
  } finally {
    if (timer) {
      clearTimeout(timer);
    }
  }
}

const skip = prerequisitesMissing();

test('the client drives a real latch serve end to end', { skip: skip ?? false }, async t => {
  const harness = startHarness();
  const gateway = await startGateway({ harness });
  t.after(() => {
    gateway.stop();
    harness.dispose();
  });
  const client = createLatchClient({ url: gateway.url, token: gateway.token });

  await t.test('sessions and capabilities carry the documented shape', async () => {
    const list = await client.listSessions();
    const summary = list.sessions.find(session => session.id === harness.sessionId);
    assert.ok(summary, 'created session is listed');
    assert.equal(summary?.command_label, 'claude');

    const inspected = await client.inspectSession({ sessionId: harness.sessionId });
    assert.equal(inspected.id, harness.sessionId);
    assert.equal(inspected.initial_size.cols, 100);

    const gatewayCapabilities = await client.gatewayCapabilities();
    assert.equal(gatewayCapabilities.endpoints.events, true);
    assert.equal(gatewayCapabilities.endpoints.send, true);
    assert.ok(gatewayCapabilities.protocolVersion >= 1);

    // How a chat client learns a session has a connector before it opens a
    // socket that would only close with 4408.
    const capabilities = await client.sessionCapabilities({ sessionId: harness.sessionId });
    assert.equal(capabilities.events.ok, true);
    assert.equal(capabilities.events.connectorEpoch, 1);
  });

  await t.test('events resume from a cursor and re-sync a stale one', async () => {
    const resumed = await collectEvents({
      client,
      sessionId: harness.sessionId,
      cursor: 4,
      count: 1
    });
    assert.equal(resumed.events[0]?.type, 'assistant_message');
    assert.equal(resumed.resyncs, 0);

    // A cursor past the end is the shape a connector-epoch change takes on the
    // wire: the gateway closes 4422 and the client restarts from zero.
    const stale = await collectEvents({
      client,
      sessionId: harness.sessionId,
      cursor: 99,
      count: 1
    });
    assert.equal(stale.resyncs, 1);
    assert.equal(stale.events[0]?.type, 'user_message');
    assert.equal(stale.events[0]?.connectorEpoch, 1);
  });

  await t.test('a refusal is a typed 409 and a permitted send is applied', async () => {
    // Half-typed composer: the screen-derived preflight says no, and the
    // endpoint is the authority that says the same thing with a reason.
    harness.setScreen('conversation\n\n  \u276f cargo test\n  ? for shortcuts\n');
    const blocked = await client.canSend({ sessionId: harness.sessionId });
    assert.equal(blocked.canSend.ok, false);
    assert.equal(blocked.sendMessage, false);

    const refusal = await client
      .send({ sessionId: harness.sessionId, message: 'hello' })
      .then(() => null)
      .catch((error: unknown) => error);
    assert.ok(refusal instanceof LatchSendError, `refusal is typed, got ${String(refusal)}`);
    assert.equal(refusal.status, 409);
    assert.equal(refusal.refused, true);
    assert.match(refusal.reason, /composer/);

    harness.setScreen('conversation\n\n  \u276f   \n  ? for shortcuts\n');
    const preflight = await client.canSend({ sessionId: harness.sessionId });
    assert.equal(preflight.canSend.ok, true);
    assert.equal(preflight.sendMessage, true);

    const report = await client.send({ sessionId: harness.sessionId, message: 'continue' });
    assert.equal(report.sent, true);
    assert.equal(report.operation, 'message');
  });

  await t.test('the terminal socket relays bytes in both directions', async () => {
    const terminal = client.attachTerminal({ sessionId: harness.sessionId, cols: 80, rows: 24 });
    const decoder = new TextDecoder();
    let seen = '';
    const echoed = new Promise<void>(resolveEcho => {
      terminal.onData(bytes => {
        seen += decoder.decode(bytes);
        if (seen.includes('<echo>ping')) {
          resolveEcho();
        }
      });
    });
    await new Promise<void>(resolveOpen => {
      terminal.onState(state => {
        if (state === 'open') {
          resolveOpen();
        }
      });
    });
    // The PTY is in canonical mode, so the far side reads a line at a time.
    terminal.write(new TextEncoder().encode('ping\r'));
    await withDeadline({
      work: echoed,
      ms: 15_000,
      message: () => `no echo; saw ${JSON.stringify(seen)}`
    });
    assert.match(seen, /<attached>/);
    terminal.close();
  });
});
