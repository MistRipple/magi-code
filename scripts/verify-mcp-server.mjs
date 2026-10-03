#!/usr/bin/env node
/**
 * Magi MCP 服务端到端验收（真实 daemon，原生 Node，无额外依赖）。
 *
 * 前置：daemon 已在运行（`MAGI_STATE_ROOT=<tmp> MAGI_PORT=<port> MAGI_OPEN_BROWSER=0 magi-daemon-app`），
 * 且 `magi-mcp` 已构建。用法：
 *
 *   node scripts/verify-mcp-server.mjs --base http://127.0.0.1:38199 \
 *     --mcp-bin target/debug/magi-mcp [--keep]
 *
 * 覆盖：注册工作区 → 启用服务 → 创建令牌 → HTTP 与 stdio 两条链路的
 * initialize / tools/list / 只读调用 / 越界拒绝 / 写入审批（允许、拒绝、超时）/ 变更列表 / 吊销。
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import readline from 'node:readline';

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : fallback;
};
const base = flag('--base', 'http://127.0.0.1:38123').replace(/\/$/, '');
const mcpBin = flag('--mcp-bin', 'target/debug/magi-mcp');

let failures = 0;
const check = (name, condition, detail = '') => {
  console.log(`${condition ? 'PASS' : 'FAIL'}  ${name}${!condition && detail ? `\n      ${detail}` : ''}`);
  if (!condition) failures += 1;
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function api(method, route, body) {
  const response = await fetch(`${base}${route}`, {
    method,
    headers: { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try { json = JSON.parse(text); } catch { /* 非 JSON */ }
  return { status: response.status, json, text };
}

let rpcId = 0;
async function httpRpc(url, secret, method, params) {
  rpcId += 1;
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(secret ? { authorization: `Bearer ${secret}` } : {}),
    },
    body: JSON.stringify({ jsonrpc: '2.0', id: rpcId, method, params }),
  });
  return { status: response.status, body: response.status === 200 ? await response.json() : null };
}

const isToolError = (reply) => reply.body?.result?.isError === true;
const toolText = (reply) => reply.body?.result?.content?.map((item) => item.text).join('\n') ?? '';

/** 在等待 tools/call 的同时，处理 Magi 界面里的外部待确认。 */
async function callWithDecision(url, secret, name, args_, decision) {
  const pending = httpRpc(url, secret, 'tools/call', { name, arguments: args_ });
  let resolved = false;
  for (let attempt = 0; attempt < 100 && !resolved && decision; attempt += 1) {
    await sleep(100);
    const listed = await api('GET', '/api/mcp-server/approvals');
    const approval = listed.json?.approvals?.[0];
    if (approval) {
      await api('POST', '/api/mcp-server/approvals/resolve', { approvalId: approval.approvalId, decision });
      resolved = true;
    }
  }
  return pending;
}

const workspaceDir = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-mcp-verify-'));
fs.writeFileSync(path.join(workspaceDir, 'hello.txt'), 'hello from workspace');
const tokenFile = path.join(workspaceDir, '..', `magi-mcp-token-${process.pid}`);

try {
  const health = await fetch(`${base}/health`).catch(() => null);
  check('daemon 可达', health?.ok === true, `无法访问 ${base}/health`);
  if (!health?.ok) process.exit(1);

  const registered = await api('POST', '/api/workspaces/register', { path: workspaceDir });
  const workspaceId = registered.json?.workspaceId ?? registered.json?.workspace?.workspaceId;
  check('注册工作区', registered.status === 200 && !!workspaceId, registered.text);

  const enabled = await api('POST', '/api/mcp-server/enabled', { enabled: true });
  check('启用 MCP 服务并得到固定端口', enabled.json?.running === true && !!enabled.json?.port, enabled.text);
  const url = enabled.json.url;
  const stdioEndpoint = enabled.json.stdioEndpoint;
  check('stdio 端点可用', !!stdioEndpoint);

  const created = await api('POST', '/api/mcp-server/tokens', {
    clientName: 'verify-script', workspaceId, profile: 'edit', ttlDays: 1,
  });
  const secret = created.json?.secret;
  check('创建令牌（原文只在创建时返回）', created.status === 200 && secret?.startsWith('magi_mcp_'), created.text);
  const statusAfter = await api('GET', '/api/mcp-server/status');
  check('状态与列表不回显令牌原文', !statusAfter.text.includes(secret));

  // ── HTTP 链路 ────────────────────────────────────────────────────────────────
  check('无令牌 → 401', (await httpRpc(url, null, 'ping', {})).status === 401);
  check('错误令牌 → 401', (await httpRpc(url, 'magi_mcp_wrong', 'ping', {})).status === 401);
  const init = await httpRpc(url, secret, 'initialize', {
    protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'verify', version: '1' },
  });
  check('initialize', init.body?.result?.serverInfo?.name === 'magi', JSON.stringify(init.body));
  const tools = await httpRpc(url, secret, 'tools/list', {});
  const names = (tools.body?.result?.tools ?? []).map((tool) => tool.name);
  check('edit 档目录含写入、不含 shell', names.includes('magi.fs.write') && !names.includes('magi.shell.exec'), names.join(','));

  const read = await httpRpc(url, secret, 'tools/call', { name: 'magi.fs.read', arguments: { path: 'hello.txt' } });
  check('只读调用无需确认', !isToolError(read) && toolText(read).includes('hello from workspace'), toolText(read));
  const escape = await httpRpc(url, secret, 'tools/call', { name: 'magi.fs.read', arguments: { path: '../outside.txt' } });
  check('越出工作区被拒', isToolError(escape), toolText(escape));

  const allowed = await callWithDecision(url, secret, 'magi.fs.write',
    { path: 'written.txt', content: 'via mcp' }, 'allow_once');
  check('写入经确认后成功', !isToolError(allowed) && fs.readFileSync(path.join(workspaceDir, 'written.txt'), 'utf8') === 'via mcp', toolText(allowed));

  const denied = await callWithDecision(url, secret, 'magi.fs.write',
    { path: 'denied.txt', content: 'no' }, 'deny');
  check('拒绝后未写入', isToolError(denied) && !fs.existsSync(path.join(workspaceDir, 'denied.txt')), toolText(denied));

  const changes = await httpRpc(url, secret, 'tools/call', { name: 'magi.changes.list', arguments: {} });
  check('变更账本列出本客户端的写入', !isToolError(changes) && toolText(changes).includes('written.txt'), toolText(changes));

  const audit = await api('GET', '/api/mcp-server/audit?limit=20');
  check('审计记录了调用且不含令牌', (audit.json?.entries?.length ?? 0) > 0 && !audit.text.includes(secret));

  // ── stdio 链路（真实中继二进制）────────────────────────────────────────────
  if (fs.existsSync(mcpBin)) {
    fs.writeFileSync(tokenFile, secret, { mode: 0o600 });
    const child = spawn(mcpBin, ['--stdio', '--endpoint', stdioEndpoint, '--token-file', tokenFile], {
      stdio: ['pipe', 'pipe', 'inherit'],
    });
    const lines = readline.createInterface({ input: child.stdout });
    const replies = new Map();
    lines.on('line', (line) => {
      try { const value = JSON.parse(line); replies.set(value.id, value); } catch { /* 忽略 */ }
    });
    const send = async (id, method, params) => {
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
      for (let i = 0; i < 100 && !replies.has(id); i += 1) await sleep(50);
      return replies.get(id);
    };
    const listed = await send(9001, 'tools/list', {});
    check('stdio：tools/list', (listed?.result?.tools ?? []).some((tool) => tool.name === 'magi.fs.read'), JSON.stringify(listed));
    const stdioRead = await send(9002, 'tools/call', { name: 'magi.fs.read', arguments: { path: 'hello.txt' } });
    check('stdio：只读调用', stdioRead?.result?.isError !== true && JSON.stringify(stdioRead).includes('hello from workspace'));
    child.stdin.end();
    child.kill();

    const bad = spawn(mcpBin, ['--stdio', '--endpoint', stdioEndpoint], {
      env: { ...process.env, MAGI_MCP_TOKEN: 'magi_mcp_wrong' }, stdio: ['pipe', 'pipe', 'inherit'],
    });
    let badOut = '';
    bad.stdout.on('data', (chunk) => { badOut += chunk; });
    await sleep(500);
    check('stdio：错误令牌被拒并断开', badOut.includes('unauthorized'), badOut);
    bad.kill();
  } else {
    console.log(`SKIP  stdio 链路：找不到 ${mcpBin}（先 cargo build -p magi-mcp-server --bin magi-mcp）`);
  }

  // ── 吊销 ─────────────────────────────────────────────────────────────────────
  const tokenId = created.json.token.tokenId;
  await api('DELETE', `/api/mcp-server/tokens/${tokenId}`);
  check('吊销后立即 401', (await httpRpc(url, secret, 'ping', {})).status === 401);

  await api('POST', '/api/mcp-server/enabled', { enabled: false });
  const off = await fetch(url, { method: 'POST' }).then(() => false).catch(() => true);
  check('关闭服务后端口不可达', off);
} finally {
  fs.rmSync(tokenFile, { force: true });
  if (!args.includes('--keep')) fs.rmSync(workspaceDir, { recursive: true, force: true });
}

console.log(failures === 0 ? '\nALL PASSED' : `\n${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
