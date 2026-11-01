// 真实使用的缓存命中报告：只读 daemon 的用量账本，不启动 daemon、不访问网络、不改任何状态。
//
// 用法：
//   node scripts/report-cache-usage.mjs [--ledger <audit-usage-ledger 目录>] [--since YYYY-MM-DD]
//                                       [--until YYYY-MM-DD] [--by model|day|both] [--json]
//
// 口径：
// - 每个传输 attempt 是一条 `model.usage.recorded`，按 eventId 去重。
// - 上游没有报告缓存读取值（`cacheReadTokens` 为空）的调用不进入命中率样本，单独计数；
//   缺失不是 miss。
// - 输入含缓存读取（`cacheReadIncludedInInput`）时按原值，否则输入 = 输入 + 读取。
// - `since` 用来比较改动落地前后：落地日期之后的数据才反映新的前缀策略。
import { readdir, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

const args = process.argv.slice(2);
const option = (name) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : undefined;
};
const ledgerDir = resolve(option('--ledger') ?? join(process.env.MAGI_STATE_ROOT ?? join(homedir(), '.magi'), 'audit-usage-ledger'));
const since = option('--since');
const until = option('--until');
const by = option('--by') ?? 'both';
const asJson = args.includes('--json');
for (const [name, value] of [['--since', since], ['--until', until]]) {
  if (value !== undefined && !/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    console.error(`${name} 需要 YYYY-MM-DD`);
    process.exit(2);
  }
}
if (!['model', 'day', 'both'].includes(by)) {
  console.error('--by 只能是 model、day 或 both');
  process.exit(2);
}

const day = (millis) => {
  const date = new Date(millis);
  const pad = (value) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
};

async function* events() {
  let names;
  try {
    names = (await readdir(ledgerDir)).filter((name) => name.endsWith('.jsonl')).sort();
  } catch (error) {
    console.error(`无法读取用量账本目录 ${ledgerDir}：${error.code ?? error.message}`);
    process.exit(1);
  }
  for (const name of names) {
    for (const line of (await readFile(join(ledgerDir, name), 'utf8')).split('\n')) {
      if (!line.includes('model.usage.recorded')) continue;
      try {
        const event = JSON.parse(line);
        if (event.event_type === 'model.usage.recorded') yield event;
      } catch { /* 跳过半写入的尾行 */ }
    }
  }
}

const empty = () => ({ calls: 0, reported: 0, unreported: 0, hitCalls: 0, input: 0, read: 0 });
const byModel = new Map();
const byDay = new Map();
const seen = new Set();
const add = (map, key, input, read, reported) => {
  const row = map.get(key) ?? empty();
  row.calls += 1;
  if (!reported) row.unreported += 1;
  else {
    row.reported += 1;
    row.input += input;
    row.read += read;
    if (read > 0) row.hitCalls += 1;
  }
  map.set(key, row);
};

for await (const event of events()) {
  const payload = event.payload ?? {};
  const id = payload.eventId ?? event.event_id;
  if (id && seen.has(id)) continue;
  if (id) seen.add(id);
  const when = day(payload.timestamp ?? event.occurred_at ?? 0);
  if ((since && when < since) || (until && when > until)) continue;
  const usage = payload.usage ?? {};
  const role = payload.callIdentity?.source ?? payload.executionBinding?.role ?? 'unknown';
  const model = payload.modelConfig?.model ?? 'unknown';
  const reported = typeof usage.cacheReadTokens === 'number';
  const read = reported ? usage.cacheReadTokens : 0;
  const input = (usage.inputTokens ?? 0) + (usage.cacheReadIncludedInInput === false ? read : 0);
  add(byModel, `${model}\t${role}`, input, read, reported);
  add(byDay, when, input, read, reported);
}

const rate = (row) => (row.input > 0 ? (100 * row.read) / row.input : null);
const rows = (map, label) => [...map.entries()].map(([key, row]) => ({ [label]: key, ...row, tokenHitRate: rate(row) }));
const modelRows = rows(byModel, 'key').map(({ key, ...rest }) => {
  const [model, role] = key.split('\t');
  return { model, role, ...rest };
}).sort((left, right) => right.input - left.input);
const dayRows = rows(byDay, 'day').sort((left, right) => left.day.localeCompare(right.day));

if (asJson) {
  console.log(JSON.stringify({ ledgerDir, since: since ?? null, until: until ?? null, models: modelRows, days: dayRows }, null, 2));
  process.exit(0);
}

const pct = (value) => (value === null ? '   n/a' : `${value.toFixed(1).padStart(5)}%`);
const num = (value) => String(value).padStart(11);
console.log(`账本 ${ledgerDir}${since ? `  起 ${since}` : ''}${until ? `  止 ${until}` : ''}`);
if (modelRows.length === 0) console.log('没有符合条件的用量记录。');
if (by !== 'day' && modelRows.length > 0) {
  console.log('\n按模型/角色（命中率只统计上游报告了缓存读取的调用）');
  console.log(`${'模型'.padEnd(34)}${'角色'.padEnd(14)}${'调用'.padStart(7)}${'未报告'.padStart(7)}${'有命中'.padStart(7)}${'输入'.padStart(12)}${'读取'.padStart(12)}  命中率`);
  for (const row of modelRows) {
    console.log(`${row.model.slice(0, 33).padEnd(34)}${row.role.padEnd(14)}${String(row.calls).padStart(7)}${String(row.unreported).padStart(7)}${String(row.hitCalls).padStart(7)}${num(row.input)} ${num(row.read)}  ${pct(row.tokenHitRate)}`);
  }
}
if (by !== 'model' && dayRows.length > 0) {
  console.log('\n按日期');
  console.log(`${'日期'.padEnd(12)}${'调用'.padStart(7)}${'输入'.padStart(12)}${'读取'.padStart(12)}  命中率`);
  for (const row of dayRows) {
    console.log(`${row.day.padEnd(12)}${String(row.calls).padStart(7)}${num(row.input)} ${num(row.read)}  ${pct(row.tokenHitRate)}`);
  }
}
const total = [...byModel.values()].reduce((sum, row) => {
  for (const key of Object.keys(sum)) sum[key] += row[key];
  return sum;
}, empty());
console.log(`\n合计 调用 ${total.calls}（未报告缓存 ${total.unreported}）输入 ${total.input} 读取 ${total.read} 命中率 ${pct(rate(total)).trim()}`);
