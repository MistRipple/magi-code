#!/usr/bin/env node
// 用法：
//   node scripts/compat/cli.mjs inspect <上游插件目录> [--kind dsh] [--require llm,tools] [--json]
//   node scripts/compat/cli.mjs prepare dsh <上游目录> --out <包目录> --integrity <sha512-…> [--archive <zip>]   （daemon 在安装期调用）
import { renderText } from './core/report.mjs';
import { createDshTranslator } from './dsh/translator.mjs';
import { registryOptions } from './core/registry-options.mjs';

// daemon 独占的管道关闭即结束工具链；崩溃时也不能留下联网重试的孤儿进程。
if (process.env.MAGI_TRANSLATOR_OWNER_PIPE === '1') {
  process.stdin.on('end', () => process.exit(1));
  process.stdin.on('error', () => process.exit(1));
  process.stdin.resume();
  process.stdin.unref?.();
}

const [command, directory, ...flags] = process.argv.slice(2);
const optionOf = (list, name) => { const i = list.indexOf(`--${name}`); return i === -1 ? undefined : list[i + 1]; };
const flag = (name) => optionOf(flags, name);
// 报告和 registry 文档可超过管道缓冲区；退出前等待完整输出，避免真实大包
// 的准入证据被截成无效 JSON。daemon 和独立检查沿同一输出合同。
const writeOutput = value => new Promise((resolve, reject) => {
  process.stdout.write(value, error => error ? reject(error) : resolve());
});

if (command === 'resolve') {
  try {
    const [{ default: pacote }, { default: semver }, { dshEnvironment, checkDshCompatibility }, { dirname }, { fileURLToPath }] = await Promise.all([
      import('pacote'), import('semver'), import('./core/dsh-compatibility.mjs'), import('node:path'), import('node:url'),
    ]);
    const spec = flags[0] ?? 'latest';
    if (!/^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/.test(directory ?? '')
      || (!semver.validRange(spec) && !/^[a-z][a-z0-9._-]*$/i.test(spec))) throw new Error('npm 插件来源或版本范围无效');
    const document = await pacote.manifest(`${directory}@${spec}`, registryOptions(process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org'));
    if (document.name !== directory || !semver.valid(document.version)) throw new Error('registry 返回的插件身份无效');
    const policy = process.env.MAGI_DSH_COMPATIBILITY_POLICY ? JSON.parse(process.env.MAGI_DSH_COMPATIBILITY_POLICY) : undefined;
    checkDshCompatibility(document, dshEnvironment(dirname(fileURLToPath(import.meta.url))), policy);
    await writeOutput(JSON.stringify({ document }));
  } catch (error) {
    await writeOutput(JSON.stringify({ error: { code: error.code ?? 'resolve-failed', message: error.message, diagnostic: error.message,
      ...(error.incompatible ? { incompatible: error.incompatible } : {}) } }));
  }
  process.exit(0);
}
if (command === 'prepare') {
  const kind = directory;
  const [upstreamDir, ...args] = flags;
  const need = (name) => optionOf(args, name) ?? (console.error(`缺少 --${name}`), process.exit(2));
  if (kind !== 'dsh' || !upstreamDir) { console.error('用法: prepare dsh <上游目录> --out <包目录> --integrity <sha512-…>'); process.exit(2); }
  try {
    const [{ prepareDsh }, { resolve, dirname }, { fileURLToPath }] = await Promise.all([import('./install/prepare.mjs'), import('node:path'), import('node:url')]);
    const result = await prepareDsh({
      upstreamDir: resolve(upstreamDir), outDir: resolve(need('out')), sharedRoot: dirname(fileURLToPath(import.meta.url)),
      source: { integrity: need('integrity'), ...(optionOf(args, 'github') ? { github: JSON.parse(optionOf(args, 'github')) } : {}) },
      compatibility: process.env.MAGI_DSH_COMPATIBILITY_POLICY ? JSON.parse(process.env.MAGI_DSH_COMPATIBILITY_POLICY) : undefined,
    });
    const archive = optionOf(args, 'archive');
    if (archive) {
      // 同一份打包实现（可复现 ZIP）：预览与安装各翻译一次，得到的摘要必须一致。
      const { packPlugin } = await import('./core/pack.mjs');
      packPlugin(resolve(need('out')), resolve(archive));
    }
    await writeOutput(JSON.stringify({ id: result.manifest.id, version: result.manifest.version, report: result.report }));
    process.exit(0);
  } catch (error) { console.error(error.message); process.exit(1); }
}
if (command !== 'inspect' || !directory) {
  console.error('用法: node scripts/compat/cli.mjs inspect <目录> [--kind dsh] [--require llm,tools] [--json]\n      node scripts/compat/cli.mjs prepare dsh <上游目录> --out <包目录> --integrity <sha512-…>');
  process.exit(2);
}
const kind = flag('kind');
const required = flag('require')?.split(',').filter(Boolean);
try {
  if (kind && kind !== 'dsh') throw new Error('当前仅支持 DSH 原包检查与安装');
  const translator = createDshTranslator({ required });
  if (!await translator.detect(directory)) throw new Error('无法识别为 DSH 插件');
  const report = await translator.inspect(directory, { required });
  await writeOutput((flags.includes('--json') ? JSON.stringify(report, null, 2) : renderText(report)) + '\n');
  process.exit(report.verdict.blocked.length === 0 ? 0 : 1);
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
