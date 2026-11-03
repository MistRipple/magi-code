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
  assert.deepEqual(registry.kinds(), ['dsh']);
  assert.equal(registry.create('dsh').kind, 'dsh');
  assert.throws(() => registry.register('dsh', () => ({})), /已注册/);
  assert.throws(() => registry.create('pi'), /未注册的翻译器: pi/);
  assert.throws(() => registry.register('Bad Name', () => ({})), /名称无效/);
  assert.throws(() => new TranslatorRegistry().register('x', () => ({ kind: 'y' })).create('x'), /kind 与注册名不一致/);
});

test('按目录特征识别生态；没有把握或并列时不猜', async () => {
  const registry = createRegistry();
  assert.equal((await registry.detect(fixture)).kind, 'dsh');
  await assert.rejects(registry.detect(fileURLToPath(new URL('.', import.meta.url))), /无法识别/);
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
  assert.equal(find(report, 'ctx.llm.registerAdapter').status, 'planned');
  assert.equal(find(report, 'ctx.tools.register').status, 'planned');
  assert.equal(find(report, 'ctx.webServer.register').status, 'dropped');
  assert.equal(find(report, 'node:fs/promises').status, 'bundled');
  assert.equal(find(report, './helper').category, 'internal');
  assert.equal(find(report, '@deepseek-ai/dsh-mystery').status, 'unknown');
  assert.equal(find(report, 'tools').kind, 'injects');
  assert.equal(Object.values(report.summary).reduce((a, b) => a + b, 0), report.entries.length);
});

test('必需能力判定：只要求 llm 时，unknown 仍阻塞；工具类别按需求计入', async () => {
  const translator = createRegistry().create('dsh');
  const onlyLlm = await translator.inspect(fixture, { required: ['llm'] });
  const names = (report) => report.verdict.blocked.map((entry) => entry.name);
  assert.ok(names(onlyLlm).includes('ctx.llm.registerAdapter'));
  assert.ok(names(onlyLlm).includes('@deepseek-ai/dsh-mystery'));
  assert.ok(!names(onlyLlm).includes('ctx.tools.register'));
  const none = await translator.inspect(fixture, { required: [] });
  assert.deepEqual(names(none), ['@deepseek-ai/dsh-mystery']);
  assert.match(renderText(onlyLlm), /阻塞翻译/);
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
