#!/usr/bin/env node
// 用法：
//   node scripts/compat/cli.mjs inspect <上游插件目录> [--kind dsh] [--require llm,tools] [--json]
//   node scripts/compat/cli.mjs prepare dsh <上游目录> --out <包目录> --integrity <sha512-…> [--archive <zip>]   （daemon 在安装期调用）
import { renderText } from './core/report.mjs';
import { createRegistry } from './index.mjs';

const [command, directory, ...flags] = process.argv.slice(2);
const optionOf = (list, name) => { const i = list.indexOf(`--${name}`); return i === -1 ? undefined : list[i + 1]; };
const flag = (name) => optionOf(flags, name);

if (command === 'resolve') {
  try {
    const [{ default: pacote }, { default: semver }, { dshEnvironment, checkDshCompatibility }, { dirname }, { fileURLToPath }] = await Promise.all([
      import('pacote'), import('semver'), import('./core/dsh-compatibility.mjs'), import('node:path'), import('node:url'),
    ]);
    const spec = flags[0] ?? 'latest';
    if (!/^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/.test(directory ?? '')
      || (!semver.validRange(spec) && !/^[a-z][a-z0-9._-]*$/i.test(spec))) throw new Error('npm 插件来源或版本范围无效');
    const document = await pacote.manifest(`${directory}@${spec}`, { registry: process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org', fullMetadata: true, preferOnline: true, ignoreScripts: true, fetchRetries: 2, fetchTimeout: 30_000 });
    if (document.name !== directory || !semver.valid(document.version)) throw new Error('registry 返回的插件身份无效');
    const policy = process.env.MAGI_DSH_COMPATIBILITY_POLICY ? JSON.parse(process.env.MAGI_DSH_COMPATIBILITY_POLICY) : undefined;
    checkDshCompatibility(document, dshEnvironment(dirname(fileURLToPath(import.meta.url))), policy);
    process.stdout.write(JSON.stringify({ document }));
  } catch (error) {
    process.stdout.write(JSON.stringify({ error: { code: error.code ?? 'resolve-failed', message: error.message, diagnostic: error.message,
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
      source: { integrity: need('integrity') },
      compatibility: process.env.MAGI_DSH_COMPATIBILITY_POLICY ? JSON.parse(process.env.MAGI_DSH_COMPATIBILITY_POLICY) : undefined,
    });
    const archive = optionOf(args, 'archive');
    if (archive) {
      // 同一份打包实现（可复现 ZIP）：预览与安装各翻译一次，得到的摘要必须一致。
      const { packPlugin } = await import('./core/pack.mjs');
      packPlugin(resolve(need('out')), resolve(archive));
    }
    process.stdout.write(JSON.stringify({ id: result.manifest.id, version: result.manifest.version, report: result.report }));
    process.exit(0);
  } catch (error) { console.error(error.message); process.exit(1); }
}
if (command !== 'inspect' || !directory) {
  console.error('用法: node scripts/compat/cli.mjs inspect <目录> [--kind dsh] [--require llm,tools] [--json]\n      node scripts/compat/cli.mjs prepare dsh <上游目录> --out <包目录> --integrity <sha512-…>');
  process.exit(2);
}
const registry = createRegistry();
const kind = flag('kind');
const required = flag('require')?.split(',').filter(Boolean);
try {
  const translator = kind ? registry.create(kind, { required }) : await registry.detect(directory, { required });
  const report = await translator.inspect(directory, { required });
  console.log(flags.includes('--json') ? JSON.stringify(report, null, 2) : renderText(report));
  process.exit(report.verdict.blocked.length === 0 ? 0 : 1);
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
