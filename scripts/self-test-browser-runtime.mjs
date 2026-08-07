#!/usr/bin/env node

import { execFileSync, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import path from 'node:path';
import readline from 'node:readline';

const args = parseArgs(process.argv.slice(2));
const metadataPath = path.resolve(required(args, 'metadata'));
const browserHostRoot = path.resolve(args['browser-host-root'] ?? 'browser-host');
const metadata = JSON.parse(await readFile(metadataPath, 'utf8'));
const root = path.resolve(metadata.root);
const profilePath = await mkdtemp(path.join(tmpdir(), 'magi-browser-runtime-self-test-'));
const nodeExecutable = path.resolve(
  args['node-executable'] ?? path.join(root, ...metadata.nodeExecutablePath.split('/')),
);
const hostEntry = path.join(root, ...metadata.hostEntryPath.split('/'));
const chromiumExecutable = path.join(root, ...metadata.chromiumExecutablePath.split('/'));
const token = randomBytes(32).toString('hex');
const requireFromHost = createRequire(path.join(browserHostRoot, 'package.json'));
const { WebSocket } = requireFromHost('ws');
let child;

try {
  child = spawn(nodeExecutable, [hostEntry], {
    env: {
      ...process.env,
      MAGI_BROWSER_PROFILE_PATH: profilePath,
      MAGI_BROWSER_DOWNLOAD_PATH: path.join(profilePath, 'Downloads'),
      MAGI_BROWSER_CHROMIUM_EXECUTABLE: chromiumExecutable,
      MAGI_BROWSER_RUNTIME_VERSION: 'self-test',
      MAGI_BROWSER_HOST_VERSION: 'self-test',
      MAGI_BROWSER_PLAYWRIGHT_VERSION: metadata.playwrightVersion,
      MAGI_BROWSER_RUNTIME_EPOCH: '1',
      MAGI_BROWSER_DAEMON_PID: String(process.pid),
      MAGI_BROWSER_HOST_PORT: '0',
      MAGI_BROWSER_HOST_TOKEN: token,
      MAGI_BROWSER_HEADLESS: '1',
      MAGI_BROWSER_MAX_ACTIVE_PAGES: '2',
      MAGI_BROWSER_MAX_TABS: '4',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const startup = await readStartup(child);
  if (startup.status !== 'ready' || !Number.isInteger(startup.port)) {
    throw new Error(`Browser Host 启动失败：${JSON.stringify(startup)}`);
  }
  assertSandboxEnabled(profilePath);
  const socket = new WebSocket(`ws://127.0.0.1:${startup.port}/control`, {
    headers: { authorization: `Bearer ${token}` },
  });
  const readyMessage = waitForMessage(socket, (value) => value?.event?.type === 'ready');
  await once(socket, 'open', 10_000);
  const ready = await readyMessage;
  if (
    ready.event.payload.playwright_version !== metadata.playwrightVersion
    || ready.event.payload.chromium_version !== metadata.chromiumVersion
  ) {
    throw new Error(`Browser Host 版本握手不一致：${JSON.stringify(ready.event.payload)}`);
  }
  const response = await request(socket, metadata, {
    type: 'create_page',
    payload: {
      tab_id: 'self-test-tab',
      initial_url: 'about:blank',
      viewport: {
        width: 1280,
        height: 800,
        surface_width: 1280,
        surface_height: 800,
        device_scale_factor_millis: 1000,
        device_type: 'desktop',
      },
      navigation_revision: 0,
      snapshot_revision: 0,
    },
  });
  if (response.outcome?.status !== 'succeeded') {
    throw new Error(`Browser Host 创建页面失败：${JSON.stringify(response)}`);
  }
  await request(socket, metadata, { type: 'shutdown' });
  socket.terminate();
  await waitForExit(child, 10_000);
  process.stdout.write(`${JSON.stringify({ status: 'passed', chromiumVersion: metadata.chromiumVersion })}\n`);
} finally {
  if (child && child.exitCode === null) child.kill('SIGTERM');
  await rm(profilePath, { recursive: true, force: true });
}

function parseArgs(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index += 2) {
    const key = values[index];
    const value = values[index + 1];
    if (!key?.startsWith('--') || value === undefined) throw new Error(`无效参数：${key ?? ''}`);
    parsed[key.slice(2)] = value;
  }
  return parsed;
}

function required(values, key) {
  const value = values[key]?.trim();
  if (!value) throw new Error(`缺少参数 --${key}`);
  return value;
}

async function readStartup(processHandle) {
  const stderr = [];
  processHandle.stderr.on('data', (chunk) => stderr.push(chunk));
  const lines = readline.createInterface({ input: processHandle.stdout });
  return Promise.race([
    new Promise((accept, reject) => {
      lines.once('line', (line) => {
        lines.close();
        try {
          accept(JSON.parse(line));
        } catch (error) {
          reject(error);
        }
      });
      processHandle.once('exit', (code) => {
        reject(new Error(`Browser Host 提前退出 (${code})：${Buffer.concat(stderr).toString('utf8')}`));
      });
    }),
    timeout(30_000, 'Browser Host 启动超时'),
  ]);
}

function assertSandboxEnabled(profilePath) {
  if (process.platform === 'win32') return;
  const commands = execFileSync('/bin/ps', ['-axo', 'command'], { encoding: 'utf8' })
    .split('\n')
    .filter((line) => line.includes(profilePath));
  if (!commands.length || commands.some((line) => line.includes('--no-sandbox'))) {
    throw new Error('Browser Runtime 未以 Chromium 沙箱模式启动');
  }
}

function request(socket, metadata, command) {
  const requestId = `self-test-${Date.now()}-${Math.random()}`;
  const response = waitForMessage(socket, (value) => value?.request_id === requestId);
  socket.send(JSON.stringify({
    request_id: requestId,
    protocol_version: {
      major: metadata.protocolMajor,
      minor: metadata.protocolMinor,
    },
    command,
  }));
  return response;
}

function waitForMessage(socket, predicate) {
  return new Promise((accept, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('Browser Host 响应超时'));
    }, 10_000);
    const onMessage = (data, isBinary) => {
      if (isBinary) return;
      const value = JSON.parse(data.toString());
      if (!predicate(value)) return;
      cleanup();
      accept(value);
    };
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.off('message', onMessage);
      socket.off('error', onError);
    };
    socket.on('message', onMessage);
    socket.on('error', onError);
  });
}

function once(emitter, event, milliseconds) {
  return Promise.race([
    new Promise((accept, reject) => {
      emitter.once(event, accept);
      emitter.once('error', reject);
    }),
    timeout(milliseconds, `${event} 超时`),
  ]);
}

function waitForExit(processHandle, milliseconds) {
  if (processHandle.exitCode !== null) return Promise.resolve();
  return Promise.race([
    new Promise((accept) => processHandle.once('exit', accept)),
    timeout(milliseconds, 'Browser Host 退出超时'),
  ]);
}

function timeout(milliseconds, message) {
  return new Promise((_, reject) => setTimeout(() => reject(new Error(message)), milliseconds));
}
