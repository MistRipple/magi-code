#!/usr/bin/env node
// 用法（daemon 在安装期调用 resolve / prepare；inspect 仅供开发诊断）：
//   node scripts/compat/cli.mjs resolve <生态> <npm 包名> [版本范围]
//   node scripts/compat/cli.mjs prepare <生态> <上游目录> --out <包目录> --integrity <sha512-…> [--archive <zip>] [--github <json>]
//   node scripts/compat/cli.mjs inspect <上游插件目录> [--kind <生态>] [--require llm,tools] [--json]
// 每个生态在 ECOSYSTEMS 登记自己的适配器；没有登记的生态一律拒绝。合同见设计文档 6.5。
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

const optionOf = (list, name) => { const i = list.indexOf(`--${name}`); return i === -1 ? undefined : list[i + 1]; };
// 报告和 registry 文档可超过管道缓冲区；退出前等待完整输出，避免真实大包
// 的准入证据被截成无效 JSON。daemon 和独立检查沿同一输出合同。
const writeOutput = value => new Promise((resolve, reject) => {
  process.stdout.write(value, error => error ? reject(error) : resolve());
});

// 生态适配器：translator 做只读检查，resolve 解析 npm 版本并按该生态准入，prepare 生成标准 Magi 包。
// 兼容策略（如 DSH 的版本豁免）只由对应生态读取，不对其他生态生效。
const ECOSYSTEMS = {
  dsh: {
    createTranslator: createDshTranslator,

    async resolve(name, spec) {
      const [{ default: pacote }, { default: semver }, { dshEnvironment, checkDshCompatibility }, { dirname }, { fileURLToPath }] = await Promise.all([
        import('pacote'), import('semver'), import('./core/dsh-compatibility.mjs'), import('node:path'), import('node:url'),
      ]);
      if (!/^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/.test(name ?? '')
        || (!semver.validRange(spec) && !/^[a-z][a-z0-9._-]*$/i.test(spec))) throw new Error('npm 插件来源或版本范围无效');
      const document = await pacote.manifest(`${name}@${spec}`, registryOptions(process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org'));
      if (document.name !== name || !semver.valid(document.version)) throw new Error('registry 返回的插件身份无效');
      const policy = process.env.MAGI_DSH_COMPATIBILITY_POLICY ? JSON.parse(process.env.MAGI_DSH_COMPATIBILITY_POLICY) : undefined;
      checkDshCompatibility(document, dshEnvironment(dirname(fileURLToPath(import.meta.url))), policy);
      return { document };
    },

    async prepare(upstreamDir, args) {
      const need = (name) => optionOf(args, name) ?? (console.error(`缺少 --${name}`), process.exit(2));
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
      return { id: result.manifest.id, version: result.manifest.version, report: result.report };
    },
  },
};

const usage = () => console.error([
  '用法: node scripts/compat/cli.mjs resolve <生态> <npm 包名> [版本范围]',
  '      node scripts/compat/cli.mjs prepare <生态> <上游目录> --out <包目录> --integrity <sha512-…>',
  '      node scripts/compat/cli.mjs inspect <目录> [--kind <生态>] [--require llm,tools] [--json]',
].join('\n'));
const ecosystemOf = (name) => Object.hasOwn(ECOSYSTEMS, name ?? '') ? ECOSYSTEMS[name] : undefined;

const [command, subject, ...rest] = process.argv.slice(2);

if (command === 'resolve') {
  const ecosystem = ecosystemOf(subject);
  try {
    if (!ecosystem) throw Object.assign(new Error(`上游生态 ${subject ?? ''} 没有翻译器`), { code: 'ecosystem-unsupported' });
    await writeOutput(JSON.stringify(await ecosystem.resolve(rest[0], rest[1] ?? 'latest')));
  } catch (error) {
    await writeOutput(JSON.stringify({ error: { code: error.code ?? 'resolve-failed', message: error.message, diagnostic: error.message,
      ...(error.incompatible ? { incompatible: error.incompatible } : {}) } }));
  }
  process.exit(0);
}
if (command === 'prepare') {
  const ecosystem = ecosystemOf(subject);
  const [upstreamDir, ...args] = rest;
  if (!ecosystem || !upstreamDir) { if (!ecosystem) console.error(`上游生态 ${subject ?? ''} 没有翻译器`); usage(); process.exit(2); }
  try {
    await writeOutput(JSON.stringify(await ecosystem.prepare(upstreamDir, args)));
    process.exit(0);
  } catch (error) { console.error(error.message); process.exit(1); }
}
if (command !== 'inspect' || !subject) { usage(); process.exit(2); }

const kind = optionOf(rest, 'kind');
const required = optionOf(rest, 'require')?.split(',').filter(Boolean);
try {
  const ecosystem = ecosystemOf(kind ?? 'dsh');
  if (!ecosystem) throw new Error(`上游生态 ${kind} 没有翻译器`);
  const translator = ecosystem.createTranslator({ required });
  if (!await translator.detect(subject)) throw new Error(`无法识别为 ${translator.kind.toUpperCase()} 插件`);
  const report = await translator.inspect(subject, { required });
  await writeOutput((rest.includes('--json') ? JSON.stringify(report, null, 2) : renderText(report)) + '\n');
  process.exit(report.verdict.blocked.length === 0 ? 0 : 1);
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
