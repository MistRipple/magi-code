// 真实 daemon/Turn/工具/账本验收。默认本地夹具；--live 才转发到现有模型连接。
// 只输出合成任务、请求哈希、耗时和 usage，不保存连接密钥或完整模型请求。
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { appServerRequest } from './lib/app-server-harness.mjs';

const args = process.argv.slice(2);
const option = name => args.includes(name) ? args[args.indexOf(name) + 1] : undefined;
const live = args.includes('--live');
const smoke = args.includes('--smoke');
const sessionStats = args.includes('--session-stats');
// 同一会话内：先一个带工具调用的回合，再两个纯对话回合。用于观察回合边界处上一回合的工具历史能否被缓存复用。
const toolTurns = args.includes('--tool-turns');
const root = fileURLToPath(new URL('..', import.meta.url));
const output = resolve(option('--output') ?? await mkdtemp(join(tmpdir(), 'magi-prompt-cache-')));
const candidate = resolve(option('--candidate') ?? join(root, 'target/debug/magi-daemon-app'));
const baseline = option('--baseline') && resolve(option('--baseline'));
assert.ok(!sessionStats || (!live && !baseline && !smoke), '--session-stats requires the full controlled candidate run');
const maxCalls = Number(option('--max-calls') ?? 48);
assert.ok(Number.isSafeInteger(maxCalls) && maxCalls > 0 && maxCalls <= 96, '--max-calls must be 1..96');
await mkdir(output, { recursive: true });
const settings = live ? JSON.parse(await readFile(option('--settings') ?? join(homedir(), '.magi/settings.json'), 'utf8')) : {};
const mainModel = option('--model') ?? settings.orchestratorSessionDefaults?.model ?? 'cache-fixture';
const auxiliaryModel = option('--aux-model') ?? settings.auxiliary?.model ?? mainModel;
const effort = live ? settings.orchestratorSessionDefaults?.reasoningEffort ?? null : null;
const mainConnection = live ? settings.orchestrator : null;
const auxConnection = live ? settings.auxiliary : null;
if (live) {
  assert.equal(mainConnection?.apiProtocol, 'openai_responses', 'live benchmark currently requires a Responses main connection');
  assert.equal(auxConnection?.apiProtocol, 'openai_responses', 'live benchmark currently requires a Responses auxiliary connection');
  assert.ok(mainConnection.apiKey && auxConnection.apiKey && auxConnection.model, 'configured connections required');
}
const digest = value => createHash('sha256').update(typeof value === 'string' || Buffer.isBuffer(value) ? value : JSON.stringify(value)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let stopping = false;
async function until(test, label, timeout = 120000) {
  const start = Date.now();
  while (Date.now() - start < timeout) {
    if (stopping) throw new Error(`benchmark stopped: ${label}`);
    const result = await test(); if (result) return result;
    await pause(150);
  }
  throw new Error(`timeout: ${label}`);
}
async function unusedPort() {
  const server = createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
const calls = [], phases = [], rejectedCalls = [];
let phase, scenario, daemon, currentCalls = [], lastInputByLane = new Map();
let fixtureOrigin, fixtureWorkspace, fixtureReadTurn = false;

const emptyCache = () => ({ observedCallCount: 0, hitCallCount: 0, inputTokens: 0, readTokens: 0, writeTokens: 0, writeObservedCallCount: 0 });
function cacheFromLedger(records) {
  const cache = emptyCache();
  for (const { usage } of records) {
    const read = usage?.cacheReadTokens ?? 0, write = usage?.cacheWriteTokens ?? 0;
    cache.readTokens += read; cache.writeTokens += write;
    cache.writeObservedCallCount += Number(usage?.cacheWriteTokens != null);
    if (usage?.cacheReadTokens == null) continue;
    cache.observedCallCount++; cache.hitCallCount += Number(read > 0);
    cache.inputTokens += Math.max(0, (usage.inputTokens ?? 0) - (usage.cacheReadIncludedInInput ? read : 0)) + read + write;
  }
  return { ...cache, tokenHitRate: cache.inputTokens ? cache.readTokens / cache.inputTokens : null,
    requestHitRate: cache.observedCallCount ? cache.hitCallCount / cache.observedCallCount : null };
}
async function readLedger(stateRoot) {
  const directory = join(stateRoot, 'audit-usage-ledger');
  const entries = [];
  for (const file of (await readdir(directory)).filter(name => /^segment-.*\.jsonl$/.test(name)).sort()) {
    for (const line of (await readFile(join(directory, file), 'utf8')).split('\n').filter(Boolean)) entries.push(JSON.parse(line));
  }
  const reset = Math.max(0, ...entries.filter(entry => entry.event_type === 'usage.stats.reset').map(entry => entry.sequence));
  const records = new Map();
  for (const entry of entries) if (entry.sequence > reset && entry.event_type === 'model.usage.recorded') {
    const key = entry.payload.eventId ?? entry.event_id;
    if (sessionStats) assert.ok(!records.has(key), 'physical usage ledger must not contain duplicate attempt events');
    records.set(key, entry.payload);
  }
  return [...records.values()];
}
function verifyLedger(stats, records, label) {
  assert.equal(stats.totals.llmCallCount, records.length, `${label}: ledger call count`);
  const cache = cacheFromLedger(records), roles = {};
  if (label === 'candidate') {
    assert.deepEqual(stats.totals.cache, cache, 'candidate: ledger/cache projection');
    assert.equal(records.length, currentCalls.length, 'each HTTP attempt has exactly one usage projection');
    const wireByIdentity = new Map();
    for (const call of currentCalls) {
      const key = `${call.wireSha256}:${call.status}`;
      const bucket = wireByIdentity.get(key) ?? [];
      bucket.push(call);
      wireByIdentity.set(key, bucket);
    }
    for (const record of records) {
      const identity = record.callIdentity;
      assert.ok(identity.parentCallId && identity.transport, 'physical attempts retain logical parent and transport identity');
      const key = `${identity.transport.requestFingerprint}:${identity.transport.httpStatus}`;
      const bucket = wireByIdentity.get(key) ?? [];
      const index = bucket.findIndex(wire => !wire.usage || (
        record.usage.inputTokens === wire.usage.inputTokens
        && record.usage.outputTokens === wire.usage.outputTokens
        && (record.usage.cacheReadTokens ?? null) === wire.usage.cachedTokens
      ));
      assert.ok(index >= 0, 'usage transport identity and provider usage match a real wire request');
      assert.equal(record.executionBinding.role, { main: 'orchestrator', worker: 'worker', auxiliary: 'auxiliary' }[bucket[index].lane], 'actual main/worker/auxiliary wire lane retains its role');
      assert.equal(identity.source, record.executionBinding.role, 'call and binding roles agree');
      bucket.splice(index, 1);
    }
    for (const key of Object.keys(emptyCache())) assert.equal(stats.models.reduce((sum, model) => sum + model.totals.cache[key], 0), cache[key], `models: ${key}`);
  }
  const requiredRoles = smoke ? ['orchestrator'] : ['orchestrator', 'worker', 'auxiliary'];
  for (const role of new Set([...requiredRoles, ...records.map(record => record.executionBinding?.role).filter(Boolean)])) {
    const selected = records.filter(record => record.executionBinding?.role === role);
    assert.ok(selected.length > 0, `${label}: missing ${role} usage`);
    const items = stats.items.filter(item => item.role === role);
    assert.equal(items.reduce((sum, item) => sum + item.llmCallCount, 0), selected.length, `${label}: ${role} count`);
    if (label === 'candidate') for (const key of Object.keys(emptyCache())) {
      assert.equal(items.reduce((sum, item) => sum + item.cache[key], 0), cacheFromLedger(selected)[key], `${role}: ${key}`);
    }
    roles[role] = { calls: selected.length, failedCalls: selected.filter(record => record.status !== 'success').length, cache: cacheFromLedger(selected) };
  }
  return { verified: true, calls: records.length, cache, roles };
}

// Independently reconcile the public projection with persisted physical receipts.
function verifySessionStats(stats, records) {
  const sum = (rows, field) => rows.reduce((total, row) => total + row[field], 0);
  function verify(totals, selected, label) {
    assert.equal(totals.llmCallCount, selected.length, `${label}: attempts`);
    assert.equal(totals.tokenObservedCallCount, selected.filter(record => record.callIdentity.transport?.usageReported).length, `${label}: usage coverage`);
    assert.equal(totals.netOutputTokens, sum(selected.map(record => record.usage), 'outputTokens'), `${label}: output`);
    assert.equal(totals.inputTokens, selected.reduce((total, { usage }) => total + usage.inputTokens
      + (usage.cacheReadIncludedInInput ? 0 : usage.cacheReadTokens ?? 0) + (usage.cacheWriteTokens ?? 0), 0), `${label}: full input`);
    assert.deepEqual(totals.cache, cacheFromLedger(selected), `${label}: cache`);
    const measured = selected.filter(record => record.status === 'success' && record.callIdentity.transport?.responseTiming);
    const timing = measured.map(record => record.callIdentity.transport.responseTiming);
    const paired = timing.filter(item => item.textOutputTokens > 0 && item.contentDurationMs > 0);
    const perf = totals.performance;
    assert.equal(perf.ttftSampleCount, timing.length, `${label}: TTFT coverage`);
    assert.equal(perf.ttftTotalMs, sum(timing, 'firstContentMs'), `${label}: TTFT sum`);
    assert.equal(perf.tpsSampleCount, paired.length, `${label}: TPS coverage`);
    assert.equal(perf.textOutputTokens, sum(paired, 'textOutputTokens'), `${label}: body tokens`);
    assert.equal(perf.contentDurationMs, sum(paired, 'contentDurationMs'), `${label}: body duration`);
    const last = measured.reduce((last, record) => !last || record.timestamp >= last.timestamp ? record : last, null);
    const lastTiming = last?.callIdentity.transport.responseTiming;
    assert.equal(perf.lastObservedAt, last?.timestamp ?? 0, `${label}: latest observation`);
    assert.equal(perf.lastTtftMs, lastTiming?.firstContentMs ?? null, `${label}: latest TTFT`);
    assert.equal(perf.lastTps, lastTiming?.textOutputTokens > 0 && lastTiming.contentDurationMs > 0
      ? lastTiming.textOutputTokens * 1000 / lastTiming.contentDurationMs : null, `${label}: latest TPS`);
    for (const item of paired) {
      assert.equal(item.textOutputTokens, 12, `${label}: reasoning tokens excluded`);
      assert.ok(item.contentDurationMs >= 40, `${label}: separately flushed body chunks`);
    }
  }
  verify(stats.totals, records, 'total');
  for (const item of stats.items) {
    const selected = records.filter(record => record.executionBinding.role === item.role
      && record.executionBinding.templateId === item.templateId
      && record.executionBinding.engineId === item.engineId
      && record.executionBinding.bindingRevision === item.bindingRevision
      && record.modelConfig.model === item.resolvedModel);
    verify(item, selected, `${item.role}/${item.templateId}`);
  }
}

function fixtureEnvelope(body) {
  const messages = body.input ?? [];
  const results = messages.filter(item => item.type === 'function_call_output').map(item => {
    try { return JSON.parse(item.output); } catch { return {}; }
  });
  const tool = name => body.tools?.find(item => item.name === `magi_builtin_${name}`)?.name;
  const action = (name, arguments_) => ({ type: 'function_call', id: `fc_${calls.length}`, call_id: `call_${calls.length}`, name: tool(name), arguments: JSON.stringify(arguments_) });
  let item = { type: 'message', id: `msg_${calls.length}`, role: 'assistant', content: [{ type: 'output_text', text: 'CACHE_BENCH_DONE', annotations: [] }] };
  const requestedToolTurn = scenario === 'tool-turns' && fixtureReadTurn;
  if (body.tools?.length && (scenario === 'read-tools' || requestedToolTurn)) {
    const readCount = messages.filter(item => item.type === 'function_call' && item.name === tool('file_read')).length;
    if (readCount < 2) item = action('file_read', { path: join(fixtureWorkspace, readCount ? 'second.txt' : 'first.txt') });
  }
  if (body.tools?.length && scenario === 'worker') {
    if (tool('agent_spawn')) {
      const spawned = results.find(result => result.tool === 'agent_spawn' && result.child_task_id);
      if (!spawned) item = action('agent_spawn', {
        task_name: 'cache_reader', role: 'explorer', display_name: '缓存验收读取代理',
        goal: `读取 ${join(fixtureWorkspace, 'first.txt')} 并用一句话说明首行。CACHE_BENCH_WORKER`,
        delegation: { purpose: 'user_requested', reason: '用户明确要求使用一个只读子代理进行缓存验收' },
        context_package: { summary: '只读缓存验收', constraints: ['只读取指定文件，禁止修改'], expected_output: '首行', references: [] },
      });
      else if (!results.some(result => result.tool === 'agent_wait' && result.all_terminal)) item = action('agent_wait', { task_ids: [spawned.child_task_id], timeout_ms: 1000 });
    } else if (!messages.some(item => item.type === 'function_call' && item.name === tool('file_read'))) item = action('file_read', { path: join(fixtureWorkspace, 'first.txt') });
  }
  const lane = body.tools?.length ? (tool('agent_spawn') ? 'main' : 'worker') : 'auxiliary';
  // 当前 sample 已在 server handler 中登记；这里数的是它之前的同类请求。
  const prior = currentCalls.filter(call => call.lane === lane).length - 1;
  // 明确的合成 usage，仅用来验收传输/账本；绝不充当真实缓存服务或效果测量。
  const reasoning = { type: 'reasoning', id: `rs_${calls.length}`, summary: [], encrypted_content: `synthetic-reasoning-${calls.length}` };
  return { id: `resp_${calls.length}`, object: 'response', status: 'completed', output: [reasoning, item], usage: {
    input_tokens: 4096, output_tokens: 16, input_tokens_details: { cached_tokens: prior ? 2048 : 0 },
    ...(sessionStats ? { output_tokens_details: { reasoning_tokens: 4 } } : {}),
  } };
}
function summarizeUsage(usage) {
  if (!usage) return null;
  return {
    inputTokens: usage.input_tokens ?? usage.prompt_tokens ?? null,
    outputTokens: usage.output_tokens ?? usage.completion_tokens ?? null,
    cachedTokens: usage.input_tokens_details?.cached_tokens ?? usage.prompt_tokens_details?.cached_tokens ?? null,
    cacheReadIncludedInInput: usage.input_tokens_details?.cached_tokens != null || usage.prompt_tokens_details?.cached_tokens != null,
    cacheWriteTokens: usage.cache_write_tokens ?? usage.cache_creation_input_tokens ?? null,
    providerCost: typeof usage.cost === 'number' ? usage.cost : null,
  };
}
const provider = createServer(async (request, response) => {
  const started = performance.now();
  let sample;
  try {
    const url = new URL(request.url, 'http://fixture');
    if (request.method === 'GET' && url.pathname === '/fixture-health') {
      response.writeHead(200, { 'content-type': 'application/json' }); response.end('{"ok":true}'); return;
    }
    const connection = url.pathname.startsWith('/aux/') ? auxConnection : mainConnection;
    const leaf = url.pathname.replace(/^\/(?:main|aux)\/v1\//, '');
    if (request.method === 'GET') {
      const catalog = { data: [...new Set([mainModel, auxiliaryModel])].map(id => ({ id })) };
      response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify(catalog)); return;
    }
    if (calls.length >= maxCalls) {
      rejectedCalls.push({ phase, scenario, reason: 'global upstream call limit' });
      response.writeHead(400); response.end('{"error":{"message":"benchmark call limit"}}'); return;
    }
    // HTTP chunk 可切在中文 schema 的 UTF-8 码点中间；逐块转字符串会改坏前缀。
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const wire = Buffer.concat(chunks), raw = wire.toString('utf8');
    assert.ok(Buffer.from(raw, 'utf8').equals(wire), 'request forwarding must preserve UTF-8 bytes');
    const body = JSON.parse(raw);
    const lane = body.tools?.length ? (body.tools.some(t => t.name === 'magi_builtin_agent_spawn') ? 'main' : 'worker') : 'auxiliary';
    const input = body.input ?? [], laneKey = `${scenario}:${lane}`;
    const previous = lastInputByLane.get(laneKey) ?? [];
    let sharedItems = 0;
    while (sharedItems < Math.min(previous.length, input.length) && JSON.stringify(previous[sharedItems]) === JSON.stringify(input[sharedItems])) sharedItems++;
    lastInputByLane.set(laneKey, input);
    sample = { phase, scenario, lane, model: body.model, streaming: body.stream === true,
      requestHash: digest(body), toolsHash: digest(body.tools ?? []), cacheKey: body.prompt_cache_key ?? null,
      sessionHeader: request.headers['x-session-id'] ?? null,
      wireSha256: digest(wire), wireBytes: wire.length,
      toolNames: (body.tools ?? []).map(tool => tool.name ?? tool.function?.name ?? null),
      toolFingerprints: (body.tools ?? []).map(tool => ({ name: tool.name ?? tool.function?.name ?? null, hash: digest(tool) })),
      controlsHash: digest(Object.fromEntries(Object.entries(body).filter(([key]) => !['input', 'tools', 'prompt_cache_key'].includes(key)))),
      sharedInputItems: sharedItems, inputItems: input.length,
      reasoningInputItems: input.filter(item => item.type === 'reasoning').length,
      firstByteMs: null, firstTextMs: null, durationMs: null, status: null, usage: null,
    };
    calls.push(sample); currentCalls.push(sample);
    if (!live) {
      const envelope = fixtureEnvelope(body); sample.usage = summarizeUsage(envelope.usage); sample.status = 200;
      sample.firstByteMs = performance.now() - started;
      if (body.stream) {
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        if (sessionStats) { response.flushHeaders(); await pause(30); }
        for (const [index, item] of envelope.output.entries()) {
          const initial = item.type === 'function_call' ? {...item,arguments:''} : item.type === 'message' ? {...item,content:[]} : item;
          response.write(`event: response.output_item.added\ndata: ${JSON.stringify({type:'response.output_item.added',output_index:index,item:initial})}\n\n`);
          const text = item.type === 'message' ? item.content[0].text : '';
          if (text) {
            sample.firstTextMs = performance.now() - started;
            const parts = sessionStats ? [text.slice(0, 6), text.slice(6)] : [text];
            for (const [partIndex, delta] of parts.entries()) {
              if (partIndex) await pause(80);
              response.write(`event: response.output_text.delta\ndata: ${JSON.stringify({type:'response.output_text.delta',item_id:item.id,output_index:index,content_index:0,delta})}\n\n`);
            }
          }
          response.write(`event: response.output_item.done\ndata: ${JSON.stringify({type:'response.output_item.done',output_index:index,item})}\n\n`);
        }
        response.end(`event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', response: envelope })}\n\n`);
      } else { sample.firstTextMs = performance.now() - started; response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify(envelope)); }
    } else {
      assert.ok(['responses'].includes(leaf), 'unexpected upstream path');
      const upstream = new URL(`${connection.baseUrl.replace(/\/$/, '')}/${leaf}`);
      const upstreamHeaders = { 'content-type': 'application/json', Authorization: `Bearer ${connection.apiKey}` };
      // 保留 daemon 的分支亲和头；验收代理不能自行抹去真实网关路由输入。
      if (typeof request.headers['x-session-id'] === 'string') upstreamHeaders['X-Session-Id'] = request.headers['x-session-id'];
      sample.affinityHeaderForwarded = typeof upstreamHeaders['X-Session-Id'] === 'string';
      const upstreamResponse = await fetch(upstream, { method: 'POST', headers: upstreamHeaders, body: raw, signal: AbortSignal.timeout(120000) });
      sample.status = upstreamResponse.status;
      response.writeHead(upstreamResponse.status, { 'content-type': upstreamResponse.headers.get('content-type') ?? 'application/json' });
      let pending = '', complete = '';
      const decoder = new TextDecoder();
      for await (const chunk of upstreamResponse.body) {
        sample.firstByteMs ??= performance.now() - started;
        response.write(chunk);
        const text = decoder.decode(chunk, { stream: true });
        if (body.stream) {
          pending += text;
          let end;
          while ((end = pending.indexOf('\n')) >= 0) {
            const line = pending.slice(0, end).trim(); pending = pending.slice(end + 1);
            if (!line.startsWith('data:')) continue;
            try {
              const event = JSON.parse(line.slice(5));
              if (event.type === 'response.output_text.delta' && event.delta) sample.firstTextMs ??= performance.now() - started;
              if (event.response?.usage ?? event.usage) sample.usage = summarizeUsage(event.response?.usage ?? event.usage);
            } catch { /* SSE 分帧和非 JSON 完结标记不含 usage */ }
          }
        } else complete += text;
      }
      if (!body.stream) {
        try { const envelope = JSON.parse(complete); sample.usage = summarizeUsage(envelope.usage); if (envelope.output?.some(item => item.type === 'message')) sample.firstTextMs = performance.now() - started; } catch { /* 保留原上游响应，不虚构数据 */ }
      }
      response.end();
    }
    sample.durationMs = performance.now() - started;
  } catch {
    if (sample) sample.error = 'upstream transport failure';
    if (!response.headersSent) response.writeHead(502, { 'content-type': 'application/json' });
    response.end('{"error":{"message":"benchmark upstream failure; inspect status, no credentials logged"}}');
  } finally {
    if (sample) sample.durationMs = performance.now() - started;
  }
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
fixtureOrigin = `http://127.0.0.1:${provider.address().port}`;

async function stop() {
  if (!daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
  const child = daemon;
  await new Promise(resolve => {
    const timer = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); }, 15000);
    child.once('exit', () => { clearTimeout(timer); resolve(); }); child.kill('SIGTERM');
  });
}
process.once('SIGTERM', () => { stopping = true; void stop(); });
async function run(label, binary) {
  phase = label; currentCalls = []; lastInputByLane = new Map();
  const phaseRoot = join(output, label); await mkdir(phaseRoot, { recursive: true });
  fixtureWorkspace = join(phaseRoot, 'workspace'); await mkdir(fixtureWorkspace, { recursive: true });
  // --tool-turns 的工具回合需要有分量的工具结果，才能看出回合边界上丢掉了多少可缓存历史。
  const filler = toolTurns ? Array.from({ length: 300 }, (_, index) => `line ${index + 1}: deterministic filler text for prompt cache verification, no instructions.\n`).join('') : '';
  await writeFile(join(fixtureWorkspace, 'first.txt'), `CACHE_FIRST_LINE\nRead-only cache verification.\n${filler}`);
  await writeFile(join(fixtureWorkspace, 'second.txt'), `CACHE_SECOND_LINE\nNo external side effects.\n${filler}`);
  const port = await unusedPort(), origin = `http://127.0.0.1:${port}`;
  let logs = '';
  const result = { label, binarySha256: digest(await readFile(binary)), completed: false, measurements: [], stats: null,
    transportCalls: 0, usageSource: live ? 'configured upstream; model identity and caching behavior not independently attested' : 'synthetic fixture usage; not a provider cache measurement' };
  const startDaemon = () => {
    daemon = spawn(binary, [], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
      MAGI_PORT: String(port), MAGI_HOST: '127.0.0.1', MAGI_STATE_ROOT: join(phaseRoot, 'state'), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0',
      MAGI_BUILD_ID: `cache-${label}-${result.binarySha256.slice(0, 16)}`,
    } });
    for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-64000); });
  };
  startDaemon();
  const json = async (path, body) => {
    const response = await fetch(`${origin}${path}`, body === undefined ? {} : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    assert.ok(response.ok, `${label} ${path}: ${response.status}`); return response.json();
  };
  const rpc = (method, params) => appServerRequest(`ws://127.0.0.1:${port}/api/app-server`, method, params);
  try {
    await until(async () => { assert.equal(daemon.exitCode, null, 'daemon exited'); try { return (await fetch(`${origin}/health`)).ok; } catch { return false; } }, 'daemon startup');
    const mainConfig = { baseUrl: `${fixtureOrigin}/main/v1`, apiKey: 'local-benchmark-placeholder', apiProtocol: 'openai_responses' };
    await json('/api/settings/orchestrator/save', mainConfig);
    await json('/api/settings/auxiliary/save', { ...mainConfig, baseUrl: `${fixtureOrigin}/aux/v1`, model: auxiliaryModel });
    const { workspaceId } = await json('/api/workspaces/register', { path: fixtureWorkspace });
    const measurements = result.measurements;
    const observedSteps = new Map();
    async function turn(text, sessionId) {
      // 夹具跟随本次测试输入，避免在“先纯对话、下一轮读文件”的种子轮提前调用工具。
      fixtureReadTurn = text.includes('first.txt') && text.includes('second.txt');
      const started = performance.now();
      const accepted = await rpc('turn/start', { scope: 'workspace', workspaceId, sessionId, text,
        requestId: randomUUID(), userMessageId: randomUUID(), locale: 'zh-CN', accessProfile: 'read_only',
        ...(sessionId ? {} : { orchestratorSessionConfig: { model: mainModel, reasoningEffort: effort } }),
      });
      const snapshot = await until(async () => {
        const snapshot = await rpc('session/read', { sessionId: accepted.sessionId, includeTurns: true });
        const turn = snapshot.turns.find(turn => turn.turnId === accepted.turnId);
        if (turn && ['failed', 'cancelled'].includes(turn.status)) throw new Error(`${label}/${scenario}: turn ${turn.status}`);
        return turn?.status === 'completed' && snapshot;
      }, `${label}/${scenario} completion`, 180000);
      for (const turn of snapshot.turns) for (const [index, step] of (turn.modelSteps ?? []).entries()) {
        observedSteps.set(step.stepId, { ...step, turnSeq: turn.turnSeq, stepIndex: index });
      }
      const completedTurn = snapshot.turns.find(turn => turn.turnId === accepted.turnId);
      const executedTools = (completedTurn.items ?? []).filter(item => item.status === 'completed' && item.tool).map(item => item.tool.name);
      if (fixtureReadTurn) assert.ok(executedTools.filter(name => name === 'file_read').length >= 2, `${label}/${scenario}: both requested file reads must execute`);
      if (text.includes('不要调用工具或派发代理')) assert.deepEqual(executedTools, [], `${label}/${scenario}: pure conversation must not execute tools`);
      measurements.push({ scenario, totalTurnMs: performance.now() - started, modelSteps: completedTurn.modelSteps?.length ?? null, executedTools });
      return accepted.sessionId;
    }
    scenario = 'conversation';
    let sessionId = await turn('缓存验收。请不要调用工具或派发代理，只回复 CACHE_BENCH_DONE。');
    for (let i = 0; i < 2; i++) sessionId = await turn('继续缓存验收：不要调用工具或派发代理，只回复 CACHE_BENCH_DONE。', sessionId);
    if (toolTurns) {
      scenario = 'tool-turns';
      // 工具回合放在会话第二轮：只有已有历史的回合才带历史边界说明，这里正是检验它不破坏前缀的位置。
      let toolSession = await turn('缓存验收。请不要调用工具或派发代理，只回复 CACHE_BENCH_DONE。');
      toolSession = await turn(`请依次调用 file_read 读取 ${join(fixtureWorkspace, 'first.txt')} 和 ${join(fixtureWorkspace, 'second.txt')}，最后各复述首行。禁止写入，不使用子代理。`, toolSession);
      for (let i = 0; i < 2; i++) toolSession = await turn('继续缓存验收：不要调用工具或派发代理，只回复 CACHE_BENCH_DONE。', toolSession);
    }
    if (!smoke) {
      scenario = 'read-tools';
      await turn(`请依次调用 file_read 读取 ${join(fixtureWorkspace, 'first.txt')} 和 ${join(fixtureWorkspace, 'second.txt')}，最后各复述首行。禁止写入，不使用子代理。`);
      scenario = 'worker';
      const workerSession = await turn(`缓存验收：我明确要求使用一个 explorer 子代理读取 ${join(fixtureWorkspace, 'first.txt')} 并报告首行。主助手等待它完成后回复结果。不修改文件，不建立计划。`);
      scenario = 'auxiliary';
      for (let i = 0; i < 2; i++) {
        const started = performance.now();
        await json('/api/prompt/enhance', { sessionId: workerSession, workspaceId, prompt: '请比较项目两个模块的职责，只读分析并给出简短建议。', locale: 'zh-CN' });
        measurements.push({ scenario, totalTurnMs: performance.now() - started });
      }
    }
    // Turn 完成后还有标题/知识提取等辅助请求；等在途请求结算再读取同一账本投影。
    let previousCount = currentCalls.length, stableSince = Date.now();
    await until(() => {
      if (currentCalls.length !== previousCount || currentCalls.some(call => call.durationMs === null)) {
        previousCount = currentCalls.length; stableSince = Date.now(); return false;
      }
      return Date.now() - stableSince >= 750;
    }, 'provider requests drained', 125000);
    const stats = await json('/api/settings/stats');
    result.stats = stats;
    if (label === 'candidate') {
      const lastByLineage = new Map(), lastFrameByLineage = new Map(), reasons = {};
      let maxTailShare = 0;
      const steps = [...observedSteps.values()].sort((left, right) => left.turnSeq - right.turnSeq || left.stepIndex - right.stepIndex);
      assert.ok(steps.length > 0, 'canonical ModelStep facts are required');
      for (const step of steps) {
        const frame = step.promptFrame, binding = frame?.cacheBinding;
        assert.ok(binding && frame.historyChainHash && Number.isSafeInteger(frame.historyMessageCount), 'persisted lineage/chain facts are required');
        assert.match(binding.lineageId, /^[a-f0-9]{64}$/);
        assert.match(binding.cacheKey, /^[a-f0-9]{64}$/);
        assert.ok(currentCalls.some(call => call.cacheKey === binding.cacheKey && call.sessionHeader === binding.lineageId), 'canonical cache identity and stable branch must match the sent request and route header');
        for (const attempt of step.attempts) {
          assert.ok(attempt.transport, 'canonical HTTP attempt must retain transport facts');
          const wire = currentCalls.find(call => call.wireSha256 === attempt.transport.requestFingerprint && call.status === attempt.transport.httpStatus);
          assert.ok(wire, 'canonical transport facts match the actual wire request and HTTP status');
          assert.equal(attempt.transport.outcome, 'succeeded', 'fixture model attempts completed');
        }
        const previous = lastByLineage.get(binding.lineageId);
        if (!live) assert.equal(binding.invalidationReason, previous ? null : 'initial', 'stable fixture prefix, including appended reasoning, must not invalidate');
        if (previous && binding.invalidationReason === null) assert.equal(binding.cacheKey, previous.cacheKey, 'appending within one epoch preserves cache identity');
        if (previous && binding.invalidationReason !== null) assert.notEqual(binding.cacheKey, previous.cacheKey, 'an invalidation must rotate cache identity');
        assert.ok(Number.isSafeInteger(frame.prefixBytes) && Number.isSafeInteger(frame.tailBytes), 'persisted prefix/tail sizes are required for cache accounting');
        const previousFrame = lastFrameByLineage.get(binding.lineageId);
        if (previousFrame && binding.invalidationReason === null && frame.historyMessageCount > previousFrame.historyMessageCount) {
          // 历史只追加时，上一请求的整个前缀（工具表、规则、历史）都应仍是本请求的前缀。
          assert.ok(frame.prefixBytes >= previousFrame.prefixBytes, `append-only step must not shrink its prefix (${previousFrame.prefixBytes} -> ${frame.prefixBytes})`);
        }
        // 每次重发的易变尾部是 Magi 设计成本，必须保持在整体请求的小比例内。
        if (frame.historyMessageCount > 1) maxTailShare = Math.max(maxTailShare, frame.tailBytes / (frame.prefixBytes + frame.tailBytes));
        lastFrameByLineage.set(binding.lineageId, frame);
        const reason = binding.invalidationReason ?? 'continued';
        reasons[reason] = (reasons[reason] ?? 0) + 1;
        lastByLineage.set(binding.lineageId, binding);
      }
      assert.ok(maxTailShare < 0.35, `volatile tail is ${(maxTailShare * 100).toFixed(1)}% of a request`);
      result.lineageVerification = { verified: true, steps: steps.length, branches: lastByLineage.size, reasons, maxVolatileTailShare: Number(maxTailShare.toFixed(4)) };
    }
    if (!live) for (const lane of smoke ? ['main'] : ['main', 'worker']) {
      // 夹具不切换 Skill/权限，整个阶段的每个角色应始终得到同一个工具目录。
      const laneCalls = currentCalls.filter(call => call.lane === lane);
      assert.ok(laneCalls.some(call => call.reasoningInputItems > 0), `${lane}: reasoning output must be replayed into a later request`);
      assert.equal(new Set(laneCalls.map(call => call.toolsHash)).size, 1, `${lane}: stable fixture tool schema`);
      assert.equal(laneCalls[0].usage.cachedTokens, 0, `${lane}: explicit synthetic zero read`);
      assert.ok(laneCalls.slice(1).some(call => call.usage.cachedTokens > 0), `${lane}: explicit synthetic positive read`);
    }
    if (!live && label === 'candidate') {
      assert.ok(stats.totals.cache.observedCallCount > 0);
      if (!smoke) {
        assert.ok(stats.items.some(item => item.role === 'worker' && item.cache.observedCallCount > 0));
        for (const item of stats.items.filter(item => ['orchestrator', 'worker', 'auxiliary'].includes(item.role))) assert.ok(item.cache.observedCallCount > 0);
      }
    }
    result.health = await json('/health');
    // 账本由 daemon 批量刷盘，优雅关闭完成后再核对磁盘事实。
    await stop();
    const records = await readLedger(join(phaseRoot, 'state'));
    result.ledgerVerification = verifyLedger(stats, records, label);
    if (sessionStats) {
      verifySessionStats(stats, records);
      for (const role of ['orchestrator', 'worker']) {
        assert.ok(stats.items.some(item => item.role === role && item.performance.tpsSampleCount > 0), `${role}: real streaming performance must reach the API`);
      }
      const callsBeforeRestart = currentCalls.length;
      startDaemon();
      await until(async () => {
        assert.equal(daemon.exitCode, null, 'restarted daemon exited');
        try { return (await fetch(`${origin}/health`)).ok; } catch { return false; }
      }, 'daemon restart');
      const restored = await json('/api/settings/stats');
      assert.deepEqual(restored.totals, stats.totals, 'restart preserves aggregate measurements');
      const sessions = [];
      for (const id of new Set(records.map(record => record.sessionId))) {
        const selected = records.filter(record => record.sessionId === id);
        const scoped = await json(`/api/settings/stats?sessionId=${encodeURIComponent(id)}`);
        verifySessionStats(scoped, selected);
        sessions.push({ sessionId: id, roles: [...new Set(selected.map(record => record.executionBinding.role))], totals: scoped.totals });
      }
      assert.ok(sessions.some(session => session.roles.includes('orchestrator') && session.roles.includes('worker')), 'worker and main usage belongs to the same visible session');
      await stop();
      assert.equal(currentCalls.length, callsBeforeRestart, 'statistics recovery must not invoke the model');
      assert.equal((await readLedger(join(phaseRoot, 'state'))).length, records.length, 'recovery must not duplicate physical attempts');
      result.sessionStatsVerification = { verified: true, restartVerified: true, sessions };
    }
    result.completed = true;
    console.log(`${label}: ${currentCalls.length} HTTP calls; ledger/statistics verified`);
  } catch (error) {
    result.error = error.message;
    try { result.stats ??= await json('/api/settings/stats'); } catch { /* 保留失败轮次已有证据 */ }
    throw error;
  } finally {
    await stop();
    result.transportCalls = currentCalls.length;
    phases.push(result);
    await writeFile(join(phaseRoot, 'result.json'), JSON.stringify(result, null, 2));
    await writeFile(join(phaseRoot, 'daemon.log'), logs);
  }
}
try {
  if (baseline) await run('baseline', baseline);
  await run('candidate', candidate);
} finally {
  await stop(); provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  await writeFile(join(output, 'benchmark.json'), JSON.stringify({ mode: live ? 'live' : 'controlled', smoke, sessionStats, mainModel, auxiliaryModel, reasoningEffort: effort,
    maxCalls, providerCacheUsage: live ? 'observed only when upstream reports cached tokens' : 'synthetic',
    monetaryCost: 'unpriced unless the configured upstream reports usage.cost; no retail pricing assumed for aliases', phases, calls, rejectedCalls,
  }, null, 2));
  console.log(`Artifacts: ${output}`);
}
