// 现有连接的模型矩阵：目录 -> 单次可用性探测 -> 三轮真实对话 -> 指定模型完整对照。
// 只在 --live 下调用现有连接；凭据仅留内存，模型正文不写入矩阵报告。
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rename, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const args = process.argv.slice(2);
const option = name => args.includes(name) ? args[args.indexOf(name) + 1] : undefined;
assert.ok(args.includes('--live'), 'model matrix requires explicit --live');
const root = fileURLToPath(new URL('..', import.meta.url));
const output = resolve(option('--output') ?? await mkdtemp(join(tmpdir(), 'magi-cache-models-')));
const settingsPath = resolve(option('--settings') ?? join(homedir(), '.magi/settings.json'));
const settings = JSON.parse(await readFile(settingsPath, 'utf8'));
const connection = settings.orchestrator;
assert.equal(connection?.apiProtocol, 'openai_responses');
assert.ok(connection.apiKey && connection.baseUrl);
const baseline = option('--baseline');
const candidate = option('--candidate');
const selected = args.flatMap((arg, index) => arg === '--model' ? [args[index + 1]] : []);
const fullModels = args.flatMap((arg, index) => arg === '--full-model' ? [args[index + 1]] : []);
await mkdir(output, { recursive: true });
const headers = { 'content-type': 'application/json', Authorization: `Bearer ${connection.apiKey}` };
const catalogResponse = await fetch(`${connection.baseUrl.replace(/\/$/, '')}/models`, { headers, signal: AbortSignal.timeout(30000) });
assert.ok(catalogResponse.ok, `catalog HTTP ${catalogResponse.status}`);
const catalog = await catalogResponse.json();
const models = [...new Set(catalog.data.map(item => item.id))].sort();
for (const model of [...selected, ...fullModels]) assert.ok(models.includes(model), `model absent from catalog: ${model}`);
const report = { protocol: 'openai_responses', startedAt: new Date().toISOString(), completed: false,
  pricing: 'unknown for proxy aliases unless provider reports a cost',
  limits: { probeCallsPerModel: 1, smokeCallsPerModel: args.includes('--tool-turns') ? 28 : 12, fullCallsPerModel: (baseline || args.includes('--tool-turns')) ? 56 : 28, concurrency: 2 },
  catalog: models, models: [] };
let writes = Promise.resolve();
const save = () => {
  const snapshot = JSON.stringify(report, null, 2);
  writes = writes.then(async () => {
    await writeFile(join(output, 'matrix.json.tmp'), snapshot);
    await rename(join(output, 'matrix.json.tmp'), join(output, 'matrix.json'));
  });
  return writes;
};
const digest = value => createHash('sha256').update(value).digest('hex');
const folder = model => `${model.replace(/[^a-zA-Z0-9_-]/g, '_')}-${digest(model).slice(0, 8)}`;
function summarize(benchmark) {
  return {
    phases: benchmark.phases.map(phase => ({ label: phase.label, completed: phase.completed, error: phase.error ?? null,
      binarySha256: phase.binarySha256, transportCalls: phase.transportCalls,
      ledger: phase.ledgerVerification ?? null, measurements: phase.measurements })),
    attempts: benchmark.calls.length, rejectedCalls: benchmark.rejectedCalls.length,
    calls: benchmark.calls.map(call => ({ scenario: call.scenario, phase: call.phase, lane: call.lane, model: call.model,
      status: call.status, usage: call.usage, firstByteMs: call.firstByteMs, firstTextMs: call.firstTextMs, durationMs: call.durationMs })),
  };
}
async function benchmark(model, full) {
  const destination = join(output, folder(model), full ? 'full' : 'smoke');
  const childArgs = [join(root, 'scripts/verify-prompt-cache.mjs'), '--live', '--settings', settingsPath,
    '--model', model, '--aux-model', model, '--output', destination, '--max-calls', String(full ? ((baseline || args.includes('--tool-turns')) ? 56 : 28) : (args.includes('--tool-turns') ? 28 : 12))];
  if (!full) childArgs.push('--smoke');
  if (args.includes('--tool-turns')) childArgs.push('--tool-turns');
  if (full && baseline) childArgs.push('--baseline', resolve(baseline));
  if (candidate) childArgs.push('--candidate', resolve(candidate));
  const child = spawn(process.execPath, childArgs, { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
  let childLog = '';
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { childLog = (childLog + chunk.toString()).slice(-16000); });
  // 子脚本为每轮/传输设超时；进程级兜底只先发 SIGTERM，给 daemon 留结算时间。
  let killTimer;
  const timer = setTimeout(() => {
    child.kill('SIGTERM'); killTimer = setTimeout(() => child.kill('SIGKILL'), 30000);
  }, full ? 900000 : 240000);
  const exitCode = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  clearTimeout(timer); clearTimeout(killTimer);
  await mkdir(destination, { recursive: true });
  await writeFile(join(destination, 'harness.log'), childLog);
  try { return { exitCode, ...summarize(JSON.parse(await readFile(join(destination, 'benchmark.json'), 'utf8'))) }; }
  catch { return { exitCode, error: 'benchmark exited without complete artifact' }; }
}
async function probe(model) {
  const started = performance.now();
  try {
    const response = await fetch(`${connection.baseUrl.replace(/\/$/, '')}/responses`, {
      method: 'POST', headers, body: JSON.stringify({ model, input: [{ role: 'user', content: 'Reply only OK.' }], max_output_tokens: 256, stream: false }),
      signal: AbortSignal.timeout(45000),
    });
    let data; try { data = await response.json(); } catch { /* Only report HTTP/status, never raw upstream error text. */ }
    const available = response.ok && !data?.error && ['completed', 'incomplete'].includes(data?.status);
    return { status: available ? 'available' : 'unavailable_in_probe', httpStatus: response.status,
      responseStatus: data?.status ?? null, durationMs: performance.now() - started,
      usage: data?.usage ? { inputTokens: data.usage.input_tokens ?? null, outputTokens: data.usage.output_tokens ?? null,
        cachedTokens: data.usage.input_tokens_details?.cached_tokens ?? null } : null };
  } catch { return { status: 'probe_timeout_or_transport_failure', durationMs: performance.now() - started }; }
}
const queue = (selected.length ? models.filter(model => selected.includes(model)) : models);
const isTextModel = model => !/(?:^|[-/])(image|embedding|tts|whisper|dall-e|sora)(?:[-/]|$)/i.test(model);
report.limits.maximumUpstreamCalls = queue.filter(isTextModel).reduce((sum, model) =>
  sum + 1 + report.limits.smokeCallsPerModel + (fullModels.includes(model) ? report.limits.fullCallsPerModel : 0), 0);
let next = 0;
await Promise.all(Array.from({ length: 2 }, async () => {
  while (next < queue.length) {
    const model = queue[next++];
    const record = { model, probe: null, smoke: null, full: null };
    report.models.push(record);
    if (!isTextModel(model)) {
      record.skipped = 'non-text model; prompt cache task uses Responses text/tool execution';
      await save(); continue;
    }
    record.probe = await probe(model); await save();
    console.log(`${model}: ${record.probe.status}`);
    if (record.probe.status !== 'available') continue;
    record.smoke = await benchmark(model, false); await save();
    console.log(`${model}: smoke ${record.smoke.exitCode === 0 ? 'passed' : 'failed'}`);
    if (fullModels.includes(model) && record.smoke.exitCode === 0) {
      record.full = await benchmark(model, true); await save();
      console.log(`${model}: full ${record.full.exitCode === 0 ? 'passed' : 'failed'}`);
    }
  }
}));
report.completed = true; report.completedAt = new Date().toISOString();
report.outcome = report.models.some(model => model.smoke?.exitCode !== undefined && model.smoke.exitCode !== 0
  || model.full?.exitCode !== undefined && model.full.exitCode !== 0) ? 'validation_failed' : 'passed';
if (report.outcome === 'validation_failed') process.exitCode = 1;
await save();
console.log(`Artifacts: ${output}`);
