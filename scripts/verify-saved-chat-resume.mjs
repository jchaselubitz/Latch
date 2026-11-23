// Opt-in field fixture. Uses an empty directory, a private gateway, and a
// signed-in native CLI. Makes two short model requests.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const [agent, binary, scratch] = process.argv.slice(2);
assert(['claude', 'codex'].includes(agent) && binary && scratch,
  'Usage: node scripts/verify-saved-chat-resume.mjs claude|codex /path/to/latch /path/to/scratch');
const root = mkdtempSync(resolve(scratch, 'saved-chat-'));
const cwd = resolve(root, 'work');
mkdirSync(cwd);
const env = { ...process.env, LATCH_HOME: resolve(root, 'latch') };
for (const key of ['LATCH_SESSION_ID', 'CLAUDECODE', 'OVERLORD_MISSION_ID', 'OVERLORD_OBJECTIVE_ID', 'OVERLORD_EXECUTION_REQUEST_ID']) {
  delete env[key];
}
execFileSync('git', ['init', '-q', cwd]);
const latch = resolve(binary);
const sessionIDs = new Set();
const tmux = ['-L', 'latch-resume-fixture-' + process.pid];
const oldReply = 'LATCH_OLD_' + randomUUID().slice(0, 8);
const newReply = 'LATCH_NEW_' + randomUUID().slice(0, 8);
const tokenFile = resolve(root, 'token');
const readyFile = resolve(root, 'ready');
let gateway;
let origin;
let token;

function launchGateway() {
  rmSync(readyFile, { force: true });
  gateway = spawn(latch, ['serve', '--bind', '127.0.0.1:0', '--token-file', tokenFile, '--ready-file', readyFile],
    { env, cwd, stdio: ['ignore', 'ignore', 'ignore'] });
}
async function awaitGateway() {
  await until(() => existsSync(readyFile), 'gateway readiness');
  const readiness = JSON.parse(readFileSync(readyFile));
  origin = readiness.gatewayUrl ?? readiness.url ?? ('http://' + readiness.bind);
  token = readFileSync(tokenFile, 'utf8').trim();
}
async function stopGateway() {
  if (!gateway || gateway.exitCode !== null || gateway.signalCode !== null) return;
  const stopped = new Promise(resolve => gateway.once('exit', resolve));
  gateway.kill('SIGINT');
  await stopped;
}
async function request(path, { body, status = 200, device = 'fixture-phone', grant = 'control' } = {}) {
  const headers = {
    Authorization: 'Bearer ' + token,
    'x-latch-device-grant': grant,
    'Content-Type': 'application/json'
  };
  if (device) headers['x-latch-device-id'] = device;
  const response = await fetch(origin + path, {
    method: body ? 'POST' : 'GET', headers, body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15_000)
  });
  const result = await response.json();
  assert.equal(response.status, status, path + ' ' + (result.reason ?? ''));
  return result;
}
async function until(fn, label, seconds = 45) {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    const value = await fn();
    if (value) return value;
    await sleep(250);
  }
  throw Error('Timed out waiting for ' + label);
}
async function trustEmptyWorkspace(session) {
  const pane = 'fixture';
  const command = 'env LATCH_HOME=' + quote(env.LATCH_HOME) + ' ' + quote(latch) + ' attach ' + session;
  execFileSync('tmux', [...tmux, 'new-session', '-d', '-s', pane, '-x', '100', '-y', '25', command]);
  try {
    const trust = await until(() => {
      const output = execFileSync('tmux', [...tmux, 'capture-pane', '-p', '-t', pane], { encoding: 'utf8' });
      if (output.includes('Quick safety check') && output.includes('No, exit')) return 'claude';
      if (output.includes('Do you trust')) return 'codex';
      if (output.includes('Ask Codex') || output.includes('Claude Code v')) return 'ready';
      return null;
    }, 'native startup');
    if (trust === 'claude') {
      execFileSync('tmux', [...tmux, 'send-keys', '-t', pane, 'Down']);
      await sleep(300);
    }
    if (trust !== 'ready') {
      execFileSync('tmux', [...tmux, 'send-keys', '-t', pane, 'Enter']);
      await until(() => {
        const output = execFileSync('tmux', [...tmux, 'capture-pane', '-p', '-t', pane], { encoding: 'utf8' });
        return !output.includes('No, exit') && !output.includes('Do you trust') &&
          (output.includes('Claude Code v') || output.includes('Ask Codex'));
      }, 'accepted scratch workspace');
    }
  } finally {
    execFileSync('tmux', [...tmux, 'kill-server']);
  }
}
function quote(value) {
  return "'" + value.replaceAll("'", "'\"'\"'") + "'";
}
function connect(session) {
  const socket = new WebSocket(origin.replace('http:', 'ws:') + '/v2/sessions/' + session + '/conversation', 'latch.v2.' + token);
  let epoch;
  let writable;
  let state;
  const items = new Map();
  const operationResults = new Map();
  socket.addEventListener('message', event => {
    const frame = JSON.parse(event.data);
    if (frame.type === 'snapshot') {
      epoch = frame.operationEpoch;
      writable = frame.state?.sendMessage?.enabled;
      items.clear();
    }
    if (frame.type === 'operation_result') operationResults.set(frame.operationId, frame);
    if (frame.state) state = frame.state;
    if (frame.type === 'state_changed') writable = frame.state?.sendMessage?.enabled;
    for (const item of frame.items ?? []) items.set(item.id, item.kind);
    for (const id of frame.itemIds ?? []) items.delete(id);
  });
  return {
    async reply(marker) {
      try {
        await until(() => epoch && writable, 'writable Hub snapshot', 65);
      } catch (error) {
        const binding = existsSync(resolve(env.LATCH_HOME, 'sessions', session, 'conversation-source-binding.json'));
        console.error('Last Hub phase ' + (state?.phase ?? 'unknown') + '; native binding ' + binding);
        throw error;
      }
      const operationId = randomUUID();
      socket.send(JSON.stringify({ type: 'send_message', operationEpoch: epoch, operationId,
        text: 'Reply with exactly ' + marker + ' and do not use any tools.' }));
      const operation = await until(() => operationResults.get(operationId), 'Hub operation result');
      assert.ok(['accepted', 'queued'].includes(operation.status), operation.reason ?? operation.status);
      await until(() => this.sawReply(marker), 'Hub assistant response ' + marker, 70);
    },
    sawReply(marker) {
      return [...items.values()].some(item => item.type === 'message' && item.role === 'assistant' && item.text.includes(marker));
    },
    close() { socket.close(); }
  };
}

try {
  launchGateway();
  await awaitGateway();
  console.error('Gateway started for ' + agent);
  const chatId = randomUUID();
  const path = '/v2/saved-chats/' + chatId + '/resume';
  const original = (await request('/v2/sessions', { body: { requestId: chatId, cwd, agent } })).session.id;
  sessionIDs.add(original);
  await trustEmptyWorkspace(original);
  const first = connect(original);
  try {
    await first.reply(oldReply);
  } finally { first.close(); }
  console.error('Original response observed through the Hub');
  const originalState = await until(async () => {
    const state = await request(path);
    return state.verified && state;
  }, 'original native ID');
  assert.equal(originalState.target.agent, agent);
  assert.equal(originalState.target.cwd, cwd);
  assert.equal(originalState.canResume, false);
  await request(path, { grant: 'interact', status: 403 });
  await request(path, { device: 'another-phone', status: 404 });
  await request(path, { body: { requestId: randomUUID() }, status: 409 });
  await request('/v2/sessions/' + original + '/stop', { body: {} });
  execFileSync(latch, ['remove', original], { env, cwd });
  await stopGateway();
  launchGateway();
  await awaitGateway();
  console.error('Closed terminal removed and gateway restarted');
  const ended = await request(path);
  assert.equal(ended.canResume, true, ended.reason);
  assert.deepEqual(ended.target, originalState.target);
  // Rename only the freshly created fixture's transcript. Restore it even
  // if the assertion fails, so the host's native store stays consistent.
  const observation = JSON.parse(readFileSync(resolve(env.LATCH_HOME, 'saved-chats', 'observations', original + '.json')));
  const history = observation.Bound.history;
  const hiddenHistory = history + '.fixture-hidden';
  renameSync(history, hiddenHistory);
  try {
    assert.equal((await request(path)).canResume, false);
    await request(path, { body: { requestId: randomUUID() }, status: 409 });
  } finally {
    renameSync(hiddenHistory, history);
  }
  const workMoved = cwd + '-moved';
  renameSync(cwd, workMoved);
  assert.equal((await request(path)).canResume, false);
  renameSync(workMoved, cwd);
  const operation = randomUUID();
  const replies = await Promise.all([request(path, { body: { requestId: operation } }),
    request(path, { body: { requestId: operation } })]);
  const resumed = replies[0].session.id;
  assert.equal(replies[1].session.id, resumed);
  assert.notEqual(resumed, original);
  assert.deepEqual(replies.map(reply => reply.reused).sort(), [false, true]);
  sessionIDs.add(resumed);
  await request(path, { body: { requestId: randomUUID() }, status: 409 });
  const resumedState = await until(async () => {
    const state = await request(path);
    return state.sessionId === resumed && state.verified && state;
  }, 'resumed native ID');
  assert.deepEqual(resumedState.target, originalState.target);
  const next = connect(resumed);
  try {
    await until(() => next.sawReply(oldReply), 'restored Hub assistant message');
    await next.reply(newReply);
  } finally { next.close(); }
  const display = execFileSync(agent === 'claude' ? 'claude' : 'codex', ['--version'], { encoding: 'utf8', env });
  const version = display.match(/\b\d+\.\d+\.\d+\b/)[0];
  console.log(JSON.stringify({
    fixtureVersion: 1, kind: 'saved-chat-resume', adapter: agent, version,
    original: { terminal: 'original-terminal', nativeId: 'native-id', hubMessages: ['original-assistant'] },
    resumed: { terminal: 'resumed-terminal', nativeId: 'native-id', hubMessages: ['original-assistant', 'follow-up-assistant'] },
    gatewayRestarted: true, followUp: 'follow-up-assistant'
  }, null, 2));
} finally {
  await stopGateway();
  for (const session of sessionIDs) {
    try { execFileSync(latch, ['stop', session, '--force'], { env, cwd, stdio: 'ignore' }); } catch {}
  }
  rmSync(root, { recursive: true, force: true });
}
