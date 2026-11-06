import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { TranslatorRegistry } from './core/registry.mjs';
import { renderText } from './core/report.mjs';
import { createRegistry } from './index.mjs';

const fixture = fileURLToPath(new URL('./fixtures/mini-dsh', import.meta.url));
const cli = fileURLToPath(new URL('./cli.mjs', import.meta.url));
const find = (report, name) => report.entries.find((entry) => entry.name === name);

test('工厂按名称创建，重复注册、未知名称与非法名称都明确失败', () => {
  const registry = createRegistry();
  assert.deepEqual(registry.kinds(), ['dsh', 'pi']);
  assert.equal(registry.create('dsh').kind, 'dsh');
  assert.throws(() => registry.register('dsh', () => ({})), /已注册/);
  assert.equal(registry.create('pi').kind, 'pi');
  assert.throws(() => registry.create('nope'), /未注册的翻译器: nope/);
  assert.throws(() => registry.register('Bad Name', () => ({})), /名称无效/);
  assert.throws(() => new TranslatorRegistry().register('x', () => ({ kind: 'y' })).create('x'), /kind 与注册名不一致/);
});

test('按目录特征识别生态；没有把握或并列时不猜', async () => {
  const registry = createRegistry();
  assert.equal((await registry.detect(fixture)).kind, 'dsh');
  const { mkdtempSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const unrelated = mkdtempSync(join(tmpdir(), 'magi-detect-'));
  writeFileSync(join(unrelated, 'package.json'), JSON.stringify({ name: 'unrelated', version: '1.0.0' }));
  await assert.rejects(registry.detect(unrelated), /无法识别/);
  // 新增生态只需注册工厂：两个同分翻译器必须要求显式指定。
  const twin = (kind) => () => ({ kind, detect: async () => 7, inspect: async () => ({}) });
  const ambiguous = new TranslatorRegistry().register('a', twin('a')).register('b', twin('b'));
  await assert.rejects(ambiguous.detect(fixture), /同时匹配 a 与 b/);
});

test('静态分析：每个能力都有明确状态，未登记的上游服务为 unknown 而非静默通过', async () => {
  const report = await createRegistry().create('dsh').inspect(fixture, { required: ['llm', 'tools'] });
  assert.equal(report.upstream.name, 'mini-dsh-plugin');
  assert.equal(find(report, 'ctx.logger.warn').status, 'shim');
  assert.equal(find(report, 'ctx.effect').status, 'shim');
  assert.equal(find(report, 'ctx.llm.registerAdapter').status, 'mapped');
  assert.equal(find(report, 'ctx.tools.register').status, 'mapped');
  assert.equal(find(report, 'ctx.webServer.register').status, 'mapped');
  assert.equal(find(report, 'node:fs/promises').status, 'bundled');
  assert.equal(find(report, './helper').category, 'internal');
  assert.equal(find(report, '@deepseek-ai/dsh-mystery').status, 'unknown');
  assert.equal(find(report, 'tools').kind, 'injects');
  assert.equal(Object.values(report.summary).reduce((a, b) => a + b, 0), report.entries.length);
});

test('必需能力判定：unknown 永远阻塞；partial 与 dropped 在被要求的类别里阻塞；已映射的不阻塞', async () => {
  const translator = createRegistry().create('dsh');
  const names = (report) => report.verdict.blocked.map((entry) => entry.name);
  const onlyLlm = await translator.inspect(fixture, { required: ['llm'] });
  assert.ok(!names(onlyLlm).includes('ctx.llm.registerAdapter'), 'llm 注册已映射，不应阻塞');
  assert.ok(names(onlyLlm).includes('@deepseek-ai/dsh-mystery'), 'unknown 永远阻塞');
  const ui = await translator.inspect(fixture, { required: ['ui'] });
  assert.ok(!names(ui).includes('ctx.webServer.register'), 'webServer.register 已映射到 api.route，不应阻塞');
  assert.ok(names(ui).includes('ctx.webServer.registerFallback'), 'SPA 回退在 Magi 没有对应物，ui 类别要求时阻塞');
  const none = await translator.inspect(fixture, { required: [] });
  assert.deepEqual(names(none), ['@deepseek-ai/dsh-mystery']);
  assert.match(renderText(ui), /阻塞翻译/);
});

test('命令行：可翻译返回 0 之外的码表示阻塞，输入错误返回 2', () => {
  const run = (...args) => spawnSync(process.execPath, [cli, ...args], { encoding: 'utf8' });
  const blocked = run('inspect', fixture, '--require', 'llm');
  assert.equal(blocked.status, 1);
  assert.match(blocked.stdout, /dropped/);
  assert.equal(run('inspect', fixture, '--kind', 'dsh', '--json').status, 1);
  assert.equal(run('inspect', fixture, '--kind', 'nope').status, 2);
  assert.equal(run('bogus').status, 2);
});

test('已构建的 npm 产物（没有 src/）从 main 入口沿相对导入扫描；扫描不到任何文件时明确失败，不给出“可翻译”', async () => {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-built-'));
  mkdirSync(join(dir, 'lib'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'built-dsh', version: '1.0.0', main: 'lib/index.js', type: 'module', peerDependencies: { '@deepseek-ai/dsh-llm': '^0.2.0' } }));
  writeFileSync(join(dir, 'lib/index.js'), "import { inner } from './inner.js';\nexport function apply(ctx) { ctx.webServer.register('/x', inner); }\n");
  writeFileSync(join(dir, 'lib/inner.js'), 'export const inner = () => {};\n');
  const report = await createRegistry().create('dsh').inspect(dir, { required: [] });
  assert.equal(report.scanned.files, 2);
  assert.equal(find(report, 'ctx.webServer.register').status, 'mapped');

  const empty = mkdtempSync(join(tmpdir(), 'compat-empty-'));
  writeFileSync(join(empty, 'package.json'), JSON.stringify({ name: 'empty-dsh', version: '1.0.0', main: 'lib/index.js' }));
  await assert.rejects(createRegistry().create('dsh').inspect(empty, { required: [] }), /没有扫描到任何源文件/);
});

test('能力目录与 Magi 源码一致：钩子名/模式取自 protocol.rs，RPC 方法取自宿主与 daemon 分发；引用不存在的能力在加载时失败', async () => {
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  const { readHookCatalog, readHostMethods } = await import('./core/magi-facts.mjs');
  const hooks = readHookCatalog();
  const methods = readHostMethods();
  for (const [id, capability] of Object.entries(CAPABILITIES)) {
    if (capability.hook) {
      const spec = hooks.get(capability.hook.name);
      assert.ok(spec, `${id}：Magi 没有钩子 ${capability.hook.name}`);
      assert.ok(spec.wired.includes(capability.hook.mode), `${id}：钩子 ${capability.hook.name} 的 ${capability.hook.mode} 模式尚未接线（wired=${spec.wired}）`);
    }
    if (capability.rpc) assert.ok(methods.has(capability.rpc), `${id}：Magi 宿主没有 RPC 方法 ${capability.rpc}`);
  }
  const { rule } = await import('./core/rules.mjs');
  assert.throws(() => rule('calls', /x/, { capability: 'no.such', fidelity: 'full' }), /未登记的 Magi 能力/);
  assert.throws(() => rule('calls', /x/, { capability: 'tool.register' }), /fidelity/);
  assert.throws(() => rule('calls', /x/, {}), /capability 或 fixed/);
});

test('规则表里的每条规则都能解析，且 mapped 只来自翻译器已实现的能力', async () => {
  const { DSH_RULES, DSH_IMPLEMENTED } = await import('./dsh/catalog.mjs');
  const { resolveRule } = await import('./core/rules.mjs');
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  for (const id of DSH_IMPLEMENTED) assert.ok(CAPABILITIES[id], `已实现的能力 ${id} 不在能力目录里`);
  for (const r of DSH_RULES) {
    const resolved = resolveRule(r, DSH_IMPLEMENTED);
    if (resolved.status === 'mapped') assert.ok(DSH_IMPLEMENTED.has(r.capability), `${r.match} 标为 mapped，但翻译器尚未实现 ${r.capability}`);
    if (r.capability && r.fidelity !== 'none') assert.ok(r.source || r.note !== undefined, `${r.match} 缺少依据`);
  }
});

test('拦截类 API：tools/pre-execute 等不再被当作“已可翻译”，partial 与 dropped 如实阻塞', async () => {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-hooks-'));
  mkdirSync(join(dir, 'lib'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'hooky', version: '1.0.0', main: 'lib/index.js', type: 'module', peerDependencies: { '@deepseek-ai/dsh-llm': '^0.2.0' } }));
  writeFileSync(join(dir, 'lib/index.js'), [
    'export function apply(ctx) {',
    "  ctx.on('tools/pre-execute', async (exec, next) => next());",
    "  ctx.on('tools/execute', async (exec, next) => next());",
    "  ctx.on('agent/turn-stopping', () => {});",
    "  ctx.on(dynamicName(), () => {});",
    '}', 'function dynamicName() { return Math.random() > 2 ? "a" : "b"; }', '',
  ].join('\n'));
  const report = await createRegistry().create('dsh').inspect(dir, { required: ['hooks'] });
  assert.equal(find(report, 'tools/pre-execute').status, 'partial');
  assert.equal(find(report, 'tools/execute').status, 'dropped');
  assert.equal(find(report, 'agent/turn-stopping').status, 'dropped');
  assert.equal(find(report, '<dynamic>').status, 'unknown');
  const blocked = report.verdict.blocked.map((entry) => entry.name);
  for (const name of ['tools/pre-execute', 'tools/execute', 'agent/turn-stopping', '<dynamic>']) assert.ok(blocked.includes(name), `${name} 应阻塞`);
});

async function piFixture(files, pkg) {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join, dirname } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-pi-'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'pi-fixture', version: '1.0.0', ...pkg }));
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }
  return dir;
}

test('pi 翻译器：入口来自 pi.extensions 或 extensions/ 约定目录；上下文靠数据流识别，处理器里的第二个参数也算', async () => {
  const dir = await piFixture({
    'extensions/a.ts': [
      'import { helper } from "./lib/helper.ts";',
      'export default function (pi) {',
      '  pi.on("tool_call", async (event, ctx) => { ctx.ui.notify("x"); helper(ctx); });',
      '  pi.registerCommand("c", { description: "d", handler: async (args, context) => { context.cwd; } });',
      '}',
    ].join('\n'),
    'extensions/lib/helper.ts': 'export function helper(ctx) { ctx.sessionManager.getEntries(); }\n',
  }, { keywords: ['pi-package'], pi: {} });
  const registry = createRegistry();
  assert.equal((await registry.detect(dir)).kind, 'pi');
  const report = await registry.create('pi').inspect(dir, { required: [] });
  assert.equal(report.scanned.files, 2, '应从约定目录入口出发并沿相对导入找到 helper.ts');
  assert.equal(find(report, 'tool_call').status, 'partial');
  assert.equal(find(report, 'ctx.ui.notify').status, 'partial');
  assert.equal(find(report, 'ctx.cwd').status, 'shim', '处理器的第二个参数被识别为上下文，名字叫 context 也算');
  assert.equal(find(report, 'ctx.sessionManager.getEntries').status, 'partial', '上下文经函数实参传播到相对导入的 helper');
});

test('pi 翻译器：dropped 的会话驱动与请求改写被如实阻塞；解析不了的入口与 glob 明确失败', async () => {
  const dir = await piFixture({
    'extensions/x.ts': [
      'export default function (pi) {',
      '  pi.sendUserMessage("go");',
      '  pi.on("before_provider_request", (e) => e.payload);',
      '  pi.on("agent_before_settle", () => ({ continue: true }));',
      '  pi.on("some_unread_event", () => {});',
      '}',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: ['hooks', 'session'] });
  assert.equal(find(report, 'pi.sendUserMessage').status, 'dropped');
  assert.equal(find(report, 'before_provider_request').status, 'dropped');
  assert.equal(find(report, 'agent_before_settle').status, 'dropped');
  assert.equal(find(report, 'some_unread_event').status, 'unknown', '文档已读区域之外的事件不猜');
  assert.ok(report.verdict.blocked.length >= 4);

  const missing = await piFixture({}, { pi: { extensions: ['./nope.ts'] } });
  await assert.rejects(createRegistry().create('pi').inspect(missing), /不存在/);
  const noEntry = await piFixture({ 'README.md': 'x' }, {});
  await assert.rejects(createRegistry().create('pi').inspect(noEntry), /找不到 pi 扩展入口/);
  const badGlob = await piFixture({ 'a.ts': 'export default function (pi) {}' }, { pi: { extensions: ['./{a,b}.ts'] } });
  await assert.rejects(createRegistry().create('pi').inspect(badGlob), /尚不支持的 glob/);
});

test('规则表：pi 与 dsh 引用的能力都存在；partial/dropped 的规则必须写明原因，none 的规则必须有依据', async () => {
  const { PI_RULES, PI_SYMBOL_RULES } = await import('./pi/catalog.mjs');
  const { DSH_RULES } = await import('./dsh/catalog.mjs');
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  for (const [name, rules] of [['pi', [...PI_RULES, ...PI_SYMBOL_RULES]], ['dsh', DSH_RULES]]) {
    for (const r of rules) {
      if (r.capability) {
        assert.ok(CAPABILITIES[r.capability], `${name}: ${r.match} 引用了不存在的能力`);
        if (r.fidelity !== 'full') assert.ok(r.note, `${name}: ${r.match} 是 ${r.fidelity}，必须写明原因`);
        if (r.fidelity === 'none') assert.ok(r.source, `${name}: ${r.match} 标为 none，必须给出文档依据`);
      }
    }
  }
});

test('上下文逃逸：宿主上下文被存进对象或传给无法解析的目标时，兼容结论不成立（unknown 阻塞）', async () => {
  const dir = await piFixture({
    'extensions/e.ts': [
      'export default function (pi) {',
      '  const holder = { pi };',
      '  holder.pi.registerTool({ name: "t" });',
      '  setup(pi);',
      '}',
      'function setup(p) { p.on("tool_call", () => {}); }',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: [] });
  assert.equal(find(report, '<context-escape>').status, 'unknown');
  assert.ok(report.verdict.blocked.some((entry) => entry.name === '<context-escape>'));
  // 同文件函数 setup(pi) 里的订阅仍被传播识别
  assert.equal(find(report, 'tool_call').status, 'partial');
});

test('同名变量不是上下文：LLM 请求里的 context.messages 不会被当成宿主 API 调用', async () => {
  const dir = await piFixture({
    'extensions/n.ts': [
      'export default function (pi) {',
      '  pi.on("tool_call", () => {});',
      '}',
      'function unrelated(context) { return context.messages.length + context.tools.length; }',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: [] });
  assert.equal(report.entries.find((entry) => /messages|ctx\.tools|context\./.test(entry.name)), undefined);
});

test('路由执行层：Node 风格与 Fetch 风格处理器都按上游语义运行；JSON 还原、状态码保留、抛错回 400、超限明确失败', async () => {
  const { runRoute, MAX_ROUTE_BODY_BYTES } = await import('./runtime/routes.mjs');
  const signal = new AbortController().signal;
  const call = (definition, input = {}) => runRoute(definition, { method: 'GET', path: '/p', ...input }, { signal });

  // Node 风格：读请求体、写状态码与响应头、res.end。
  const echo = await call({
    handler: async (req, res) => {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      res.writeHead(201, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ got: body, url: req.url, method: req.method, host: req.headers.host }));
    },
  }, { method: 'POST', path: '/echo', query: 'a=1', body: { x: 1 } });
  assert.equal(echo.status, 201);
  assert.deepEqual(echo.body, { got: { x: 1 }, url: '/echo?a=1', method: 'POST', host: '127.0.0.1' });

  // 上游用 req.socket.localAddress / headers.host 做回环校验：必须是回环值。
  const loop = await call({ handler: (req, res) => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify([req.socket.localAddress, req.headers.origin ?? null])); } });
  assert.deepEqual(loop.body, ['127.0.0.1', null]);

  // 分块写出（NDJSON）整段返回为文本。
  const stream = await call({ handler: (_req, res) => { res.writeHead(200, { 'Content-Type': 'application/x-ndjson' }); res.write('{"a":1}\n'); res.write('{"b":2}\n'); res.end(); } });
  assert.equal(stream.contentType, 'application/x-ndjson');
  assert.equal(stream.body, '{"a":1}\n{"b":2}\n');

  // Fetch 风格：只服务 GET / HEAD。
  const fetched = await call({ resource: async (request) => new Response(JSON.stringify({ path: new URL(request.url).pathname }), { headers: { 'content-type': 'application/json' } }) });
  assert.deepEqual(fetched.body, { path: '/p' });
  const refused = await call({ resource: async () => new Response('x') }, { method: 'POST' });
  assert.equal(refused.status, 405);

  // 处理器抛错：与 DSH 一致回 400，宿主不失败。
  const thrown = await call({ handler: () => { throw new Error('坏了'); } });
  assert.equal(thrown.status, 400);
  assert.match(thrown.body.error, /坏了/);

  // 响应超限：明确失败，不截断。
  const oversize = await call({ handler: (_req, res) => { res.writeHead(200); res.write(Buffer.alloc(MAX_ROUTE_BODY_BYTES)); res.end(Buffer.alloc(1)); } });
  assert.equal(oversize.status, 502, '超限不得变成 200 空正文');
  assert.match(oversize.body.error, /超过/);
  // 恰好等于上限的响应是合法的。
  const exact = await call({ handler: (_req, res) => { res.writeHead(200, { 'content-type': 'text/plain' }); res.end(Buffer.alloc(MAX_ROUTE_BODY_BYTES, 97)); } });
  assert.equal(exact.status, 200);
  assert.equal(exact.body.length, MAX_ROUTE_BODY_BYTES);
});

test('路由执行层：调用被取消时立即以 Cancelled 结束，不等处理器', async () => {
  const { runRoute } = await import('./runtime/routes.mjs');
  const controller = new AbortController();
  const pending = runRoute({ handler: () => new Promise(() => {}) }, { method: 'GET', path: '/slow' }, { signal: controller.signal });
  setTimeout(() => controller.abort(), 20);
  await assert.rejects(pending, (error) => error.kind === 'Cancelled');
});

test('界面槽位：ctx 的实现只含上游入口用到的成员；未知服务缺席，未知槽位按策略分类', async () => {
  const { createSlotContext, slotDisposition } = await import('./runtime/client/slot-context.mjs');
  const scope = { getSnapshot: () => ({ value: {} }), subscribe: () => () => {}, set: async () => true };
  const slots = createSlotContext({ scope });
  assert.equal(slots.ctx.get('layout'), undefined, 'DSH 外壳服务缺席');
  assert.equal(slots.ctx.get('configForms').get('x'), scope);
  assert.throws(() => slots.ctx.uiRenderer.mount(), TypeError, '未提供的成员是 TypeError，不静默吞掉');
  const dispose = slots.ctx.slots.inject('plugins.bundle.config', () => slots.ctx.slots.register({ name: 'plugins.bundle.config' }, () => null));
  assert.equal(slots.live().length, 1);
  dispose();
  assert.equal(slots.live().length, 0);
  assert.equal(slotDisposition('plugins.bundle.config'), 'mounted');
  assert.equal(slotDisposition('plugins.row.config'), 'alternate');
  assert.equal(slotDisposition('settings.section'), 'mounted');
  assert.equal(slotDisposition('settings.section.icon'), 'alternate');
  assert.equal(slotDisposition('conversation.input.right'), 'unmounted');
  slots.ctx.locale.register('ns', { zh: { hi: '你好 {name}' }, en: { hi: 'Hi {name}' } });
  const t = slots.ctx.locale.bind('ns');
  assert.equal(t('hi', { name: 'A' }), '你好 A');
  slots.setLocale('en-US');
  assert.equal(t('hi', { name: 'A' }), 'Hi A');
  assert.equal(t('missing'), 'missing');
});

test('图标：上游声明的位图被带进清单与包；没有图标不产生；SVG、越界、伪造、超限明确失败', async () => {
  const { mkdtempSync, writeFileSync, mkdirSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { resolveUpstreamIcon } = await import('./translate/icon.mjs');
  const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(32)]);
  const make = (declared, file, bytes) => {
    const dir = mkdtempSync(join(tmpdir(), 'magi-icon-'));
    mkdirSync(join(dir, 'up/icons'), { recursive: true });
    if (file && bytes) writeFileSync(join(dir, 'up', file), bytes);
    return { upstreamDir: join(dir, 'up'), pkg: { name: 'x', ...(declared === undefined ? {} : { icon: declared }) } };
  };
  assert.deepEqual(resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', png)), { from: 'icons/a.png', to: 'ui/icon.png' });
  assert.equal(resolveUpstreamIcon(make(undefined)), null, '没有声明就没有图标');
  assert.throws(() => resolveUpstreamIcon(make('icons/a.svg', 'icons/a.svg', Buffer.from('<svg/>'))), /不受支持/);
  assert.throws(() => resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', Buffer.from('<script>'))), /内容与其扩展名不符/);
  assert.throws(() => resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', Buffer.alloc(300 * 1024, 1))), /超过 256 KiB/);
  assert.throws(() => resolveUpstreamIcon(make('../../etc/hosts')), /指向包外/);
  assert.throws(() => resolveUpstreamIcon(make('icons/missing.png')), /文件不存在/);
});
