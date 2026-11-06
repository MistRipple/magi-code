// 上游原包的安装封装。只读元数据与静态合同，不 import 上游、不执行 apply、不猜测贡献目录。
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRegistry } from '../index.mjs';
import { packageId } from '../core/identity.mjs';
import { findClientFile } from './client-assets.mjs';
import { resolveUpstreamIcon } from './icon.mjs';
import { installDependencies } from './dependencies.mjs';
import { renderSettingsPage } from './settings-page.mjs';

const COMPAT_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const TRANSLATOR_VERSION = 2;
export const magiPluginId = packageId;
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
export function sharedRuntimeId() {
  return hash(readFileSync(join(COMPAT_DIR, 'package-lock.json'))).slice(0, 24);
}
export function contributionId(name, taken) {
  const id = String(name).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').replace(/^([0-9])/, 'x-$1').slice(0, 64);
  if (!/^[a-z][a-z0-9-]*$/.test(id) || taken.has(id)) throw new Error(`贡献名称无效或冲突：${name}`);
  taken.add(id); return id;
}
function copyTree(source, target) {
  const files = [];
  let total = 0;
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name, 'en'))) {
      const full = join(directory, entry.name);
      if (entry.isSymbolicLink() || (!entry.isDirectory() && !entry.isFile())) throw new Error(`上游含链接或特殊文件：${relative(source, full)}`);
      if (entry.isDirectory()) { visit(full); continue; }
      const name = relative(source, full).split(sep).join('/');
      const size = statSync(full).size; total += size;
      if (size > 4 * 1024 * 1024 || total > 32 * 1024 * 1024 || files.length >= 4096) throw new Error('上游原包及依赖超过有界安装限制');
      const dest = join(target, 'extension/upstream', name);
      mkdirSync(dirname(dest), { recursive: true }); copyFileSync(full, dest); files.push(name);
    }
  };
  visit(source); return files;
}
function entryOf(pkg) {
  const exported = pkg.exports?.['.'] ?? (typeof pkg.exports === 'string' ? pkg.exports : undefined);
  return (typeof exported === 'string' ? exported : exported?.node?.import ?? exported?.import ?? exported?.default ?? pkg.main ?? 'lib/index.js').replace(/^\.\//, '');
}

export async function prepareDsh(options) {
  const pkg = JSON.parse(readFileSync(join(options.upstreamDir, 'package.json'), 'utf8'));
  const translator = createRegistry().create('dsh');
  if (!(await translator.detect(options.upstreamDir))) throw new Error(`${pkg.name} 不是 DSH 插件`);
  if (existsSync(options.outDir) && readdirSync(options.outDir).length) throw new Error('输出目录必须为空');
  const main = entryOf(pkg), root = resolve(options.upstreamDir), entry = resolve(root, main);
  if (!entry.startsWith(root + sep) || !existsSync(entry) || !/\.(?:mjs|cjs|js)$/.test(main)) throw new Error('上游入口无效或尚未构建');
  const inspection = await translator.inspect(root, { required: ['llm', 'tools', 'commands', 'settings', 'runtime', 'hooks', 'session', 'services', 'prompt', 'native'] });
  if (inspection.verdict.blocked.length) throw new Error(`上游使用了尚未实现的必需契约：\n${inspection.verdict.blocked.map((item) => `${item.name}: ${item.note}`).join('\n')}`);
  const dependencies = await installDependencies(root, options.sharedRoot);
  const manifest = {
    sdkVersion: 2, id: packageId('dsh', pkg.name), version: pkg.version,
    name: pkg.displayName ?? pkg.name, description: pkg.description ?? '', dataSchemaVersion: 1,
    extension: { hooks: [], compat: { kind: 'dsh' }, runtimeContributions: ['tools', 'commands', 'modelSources', 'settings'] },
    permissions: [
      { kind: 'extension', targets: ['full-trust'] }, { kind: 'storage', targets: ['__settings'] },
      { kind: 'tools', targets: ['*'] }, { kind: 'modelSources', targets: ['*'] },
    ],
    settingsSchema: { type: 'object', additionalProperties: true },
    contributions: { views: [{ id: 'settings', title: '设置', entry: 'ui/settings.html', placements: ['settings'] }] },
  };
  mkdirSync(options.outDir, { recursive: true });
  const kept = copyTree(root, options.outDir);
  const runtimeId = sharedRuntimeId();
  writeFileSync(join(options.outDir, 'extension/index.mjs'), `import { activateDsh } from '@magi/compat-dsh';\nimport * as upstream from ${JSON.stringify(`./upstream/${main}`)};\nexport default api => activateDsh(api, upstream, ${JSON.stringify({ runtimeVersion: TRANSLATOR_VERSION, plugin: pkg.name, runtimeId })});\n`);
  mkdirSync(join(options.outDir, 'ui'), { recursive: true });
  const client = findClientFile(root, pkg);
  if (client) {
    if (!/^window\.__ModuleLoader__\.load\(/.test(readFileSync(client.file, 'utf8'))) throw new Error('不支持的 DSH 客户端模块格式');
    copyFileSync(join(COMPAT_DIR, 'dist/dsh-client-host.js'), join(options.outDir, 'ui/host.js'));
    copyFileSync(client.file, join(options.outDir, 'ui/client.js'));
    writeFileSync(join(options.outDir, 'ui/dsh-config.js'), 'globalThis.__MAGI_DSH_CLIENT__ = { runtimeVersion: 2, clientScript: "client.js" };\n');
    writeFileSync(join(options.outDir, 'ui/settings.html'), '<!doctype html>\n<meta charset="utf-8"><meta name="color-scheme" content="light dark">\n<div id="root"></div><script src="dsh-config.js"></script><script src="host.js"></script>\n');
  } else {
    writeFileSync(join(options.outDir, 'ui/settings.html'), renderSettingsPage({ schema: manifest.settingsSchema, title: manifest.name, runtimeSchema: true }));
  }
  const icon = resolveUpstreamIcon({ upstreamDir: root, pkg });
  if (icon) { copyFileSync(join(root, icon.from), join(options.outDir, icon.to)); manifest.icon = icon.to; }
  writeFileSync(join(options.outDir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  const report = {
    kind: 'dsh', translatorVersion: TRANSLATOR_VERSION, runtimeId,
    upstream: { name: pkg.name, version: pkg.version, integrity: options.source.integrity },
    magi: { id: manifest.id, version: manifest.version }, dependencies,
    runtimeContributions: manifest.extension.runtimeContributions,
    tools: [], commands: [], modelSources: [], routes: [], listeners: [], variants: [],
    ui: { mode: client ? 'native' : 'generated', mountedSlots: [], unmountedSlots: [] },
    static: { summary: inspection.summary, degraded: inspection.entries.filter((item) => ['partial', 'dropped'].includes(item.status)) },
    files: { kept: kept.length, omitted: [] },
  };
  writeFileSync(join(options.outDir, 'extension/compat.json'), JSON.stringify(report, null, 2) + '\n');
  return { manifest, report };
}
