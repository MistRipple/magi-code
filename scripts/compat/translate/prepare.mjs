// 上游原包的安装封装。静态扫描先做准入；随后在受限记录器中启动一次真实
// Cordis，只记录准入证据。安装后的原包由 daemon 管理的 Node 宿主运行，
// 动态贡献仍在运行期发布，不把记录器输出当作完整业务验收。
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRegistry } from '../index.mjs';
import { packageId } from '../core/identity.mjs';
import { analyzeNativeClient, findClientFile, usesComposerClient } from './client-assets.mjs';
import { resolveUpstreamIcon } from './icon.mjs';
import { installDependencies } from './dependencies.mjs';
import { renderSettingsPage } from './settings-page.mjs';
import { copyBrowserAssets } from './browser-assets.mjs';
import { recordUpstream } from './record.mjs';
import { runtimeId } from '../core/identity.mjs';
import { summarize, verdictFor } from '../core/report.mjs';
import { classify as classifyDsh } from '../dsh/catalog.mjs';
import { clientViews } from './client-views.mjs';
import { dshEnvironment, checkDshCompatibility } from '../core/dsh-compatibility.mjs';
import { translateClientBusiness } from './client-business.mjs';

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
      if ((name.startsWith('native/windows/') && process.platform !== 'win32')
        || (name.startsWith('native/macos/') && process.platform !== 'darwin')
        || (name.startsWith('native/linux/') && process.platform !== 'linux')) continue;
      if (/\.(node|wasm)$/i.test(name)) throw new Error(`上游原包或依赖包含未支持的原生模块：${name}`);
      const size = statSync(full).size; total += size;
      const limit = name.startsWith('native/') ? 64 * 1024 * 1024 : 4 * 1024 * 1024;
      if (size > limit || total > 64 * 1024 * 1024 || files.length >= 4096) throw new Error('上游原包及依赖超过有界安装限制');
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
  const compatibility = checkDshCompatibility(pkg, dshEnvironment(options.sharedRoot), options.compatibility);
  if (existsSync(options.outDir) && readdirSync(options.outDir).length) throw new Error('输出目录必须为空');
  const main = entryOf(pkg), root = resolve(options.upstreamDir), entry = resolve(root, main);
  if (!entry.startsWith(root + sep) || !existsSync(entry) || !/\.(?:mjs|cjs|js)$/.test(main)) throw new Error('上游入口无效或尚未构建');
  const inspection = await translator.inspect(root, { required: ['llm', 'tools', 'commands', 'settings', 'runtime', 'hooks', 'session', 'services', 'prompt', 'native'] });
  if (inspection.verdict.blocked.length) throw new Error(`Magi 兼容检查未通过：\n${inspection.verdict.blocked.map((item) => `${item.name}（${item.status === 'unknown' ? '尚未完成兼容核对' : '宿主能力尚未接入'}）: ${item.note}`).join('\n')}`);
  const dependencies = await installDependencies(root, options.sharedRoot, undefined, compatibility);
  // 静态扫描不能证明 apply() 能启动，也看不到延迟注册的工具/事件。安装前在
  // 受限记录器中启动一次真实 Cordis：失败直接阻塞安装，成功的运行期贡献写入
  // 清单，避免“空清单但安装成功”的假阳性。
  const recorded = recordUpstream({ entry, sharedRoot: options.sharedRoot, upstreamDir: root });
  const runtimeEntries = recorded.listeners.map((name) => {
    const rule = classifyDsh('events', name);
    return { kind: 'events', name, category: rule.category, status: rule.status, target: rule.target, note: rule.note, source: rule.source, needs: rule.needs, capability: rule.capability, count: 1, files: [] };
  });
  // 对运行期发现的事件复用同一生态规则；这里通过已生成报告的分类结果查找
  // 静态规则，未知事件保持 unknown，绝不因“记录到了”而自动放行。
  for (const entry of runtimeEntries) {
    const known = inspection.entries.find((item) => item.kind === 'events' && item.name === entry.name);
    if (known) Object.assign(entry, known);
  }
  const entries = [...inspection.entries, ...runtimeEntries.filter((item) => !inspection.entries.some((known) => known.kind === item.kind && known.name === item.name))];
  const reportSummary = summarize(entries);
  const reportVerdict = verdictFor(entries, inspection.verdict.required);
  const report = { ...inspection, entries, summary: reportSummary, verdict: reportVerdict };
  if (reportVerdict.blocked.length) throw new Error(`Magi 运行期兼容检查未通过：\n${reportVerdict.blocked.map((item) => `${item.name}（${item.status}）: ${item.note}`).join('\n')}`);
  const normalizeTimeout = (value) => value == null ? undefined : Math.min(600_000, Math.max(1_000, Number(value) || 1_000));
  const toolContributions = recorded.tools.map((tool) => ({
    id: runtimeId('tool', tool.id), title: tool.id, description: tool.description ?? '',
    inputSchema: tool.parameters ?? { type: 'object' }, readOnly: tool.readOnly === true,
    ...(tool.timeoutMs == null ? {} : { timeoutMs: normalizeTimeout(tool.timeoutMs) }),
  }));
  const commandContributions = recorded.commands.map((command) => ({ id: runtimeId('cmd', command.id), title: command.id, description: command.description ?? '' }));
  const manifest = {
    sdkVersion: 2, id: packageId('dsh', pkg.name), version: pkg.version,
    name: pkg.displayName ?? pkg.name, description: pkg.description ?? '', dataSchemaVersion: 1,
    extension: { hooks: [], compat: { kind: 'dsh' }, runtimeContributions: ['tools', 'commands', 'modelSources', 'settings'] },
    permissions: [
      { kind: 'extension', targets: ['full-trust'] }, { kind: 'storage', targets: ['__settings'] },
      { kind: 'tools', targets: ['*'] }, { kind: 'modelSources', targets: ['*'] },
    ],
    settingsSchema: { type: 'object', additionalProperties: true },
    // 工具/命令由真实 Cordis 启动后通过 runtime catalog 发布；若同时写入静态
    // 清单，daemon 合并运行目录时会把同一个贡献 ID 计数两次。
    contributions: { tools: [], commands: [], views: [{ id: 'settings', title: '设置', entry: 'ui/settings.html', placements: ['settings'] }] },
  };
  const client = findClientFile(root, pkg);
  const clientSource = client ? readFileSync(client.file, 'utf8') : null;
  const business = clientSource ? translateClientBusiness(clientSource) : null;
  if (client && (business.operations.length || usesComposerClient(clientSource))) {
    manifest.permissions.push({ kind: 'composer', targets: ['images'] });
  }
  if (inspection.entries.some((entry) => /^(?:ctx\.)?(?:pluginManager|loader)(?:\.|$)/.test(entry.name))
    || ['pluginManager', 'loader'].some((name) => Object.hasOwn(inspection.serviceReads, name))) {
    manifest.permissions.push({ kind: 'pluginManagement', targets: ['install', 'lifecycle', 'authorize', 'compatibility', 'background'] });
  }
  mkdirSync(options.outDir, { recursive: true });
  const kept = copyTree(root, options.outDir);
  const compatRuntimeId = sharedRuntimeId();
  const optionalEvents = recorded.listeners.filter((name) => inspection.entries.some((item) => item.kind === 'events' && item.name === name && item.optional));
  writeFileSync(join(options.outDir, 'extension/index.mjs'), `import { activateDsh } from '@magi/compat-dsh';\nimport * as upstream from ${JSON.stringify(`./upstream/${main}`)};\nexport default api => activateDsh(api, upstream, ${JSON.stringify({ runtimeVersion: TRANSLATOR_VERSION, plugin: pkg.name, runtimeId: compatRuntimeId, optionalEvents })});\n`);
  mkdirSync(join(options.outDir, 'ui'), { recursive: true });
  let clientAnalysis = null;
  if (client) {
    if (!/window\.__ModuleLoader__\.load\(\s*\{\s*id\s*:\s*(['"])[^'"\n]+\1\s*,\s*factory\s*(?::|\()/.test(readFileSync(client.file, 'utf8'))) throw new Error('不支持的 DSH 客户端模块格式');
    clientAnalysis = analyzeNativeClient({ upstreamDir: root, pkg, sharedRoot: options.sharedRoot });
    copyFileSync(join(COMPAT_DIR, 'dist/dsh-client-host.js'), join(options.outDir, 'ui/host.js'));
    const assets = copyBrowserAssets(client.file, COMPAT_DIR, join(options.outDir, 'ui'));
    writeFileSync(join(options.outDir, 'ui/client.js'), business.code);
    const views = clientViews(clientAnalysis.mounted, manifest.name);
    manifest.contributions.views = views.map(({ slot, ...view }) => view);
    const panelViews = views.filter((view) => view.placements.includes('rightPane')).map((view) => ({ key: view.slot.key, viewId: view.id }));
    for (const { slot, id, entry } of views) {
      const configFile = `dsh-config-${id}.js`;
      writeFileSync(join(options.outDir, 'ui', configFile), `globalThis.__MAGI_DSH_CLIENT__ = ${JSON.stringify({ runtimeVersion: 2, clientScript: 'client.js', libraryScripts: assets.scripts, slotName: slot.name, slotKey: slot.key, panelViews, business: { operations: business.operations } })};\n`);
      writeFileSync(join(options.outDir, entry), `<!doctype html>\n<meta charset="utf-8"><meta name="color-scheme" content="light dark">${assets.styles.map((name) => `<link rel="stylesheet" href="${name}">`).join('')}\n<div id="root"></div><script src="${configFile}"></script><script src="host.js"></script>\n`);
    }
  } else {
    writeFileSync(join(options.outDir, 'ui/settings.html'), renderSettingsPage({ schema: manifest.settingsSchema, title: manifest.name, runtimeSchema: true }));
  }
  const icon = await resolveUpstreamIcon({ upstreamDir: root, pkg });
  if (icon) { writeFileSync(join(options.outDir, icon.to), icon.bytes); manifest.icon = icon.to; }
  writeFileSync(join(options.outDir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  const compatReport = {
    kind: 'dsh', translatorVersion: TRANSLATOR_VERSION, runtimeId: compatRuntimeId,
    upstream: { name: pkg.name, version: pkg.version, integrity: options.source.integrity },
    compatibility,
    magi: { id: manifest.id, version: manifest.version }, dependencies,
    runtimeContributions: manifest.extension.runtimeContributions,
    serviceReads: inspection.serviceReads,
    tools: recorded.tools, commands: recorded.commands, modelSources: recorded.providers,
    routes: recorded.routes, listeners: recorded.listeners, variants: recorded.variants,
    ui: { mode: client ? 'native' : 'generated', mountedSlots: clientAnalysis?.mounted ?? [], unmountedSlots: clientAnalysis?.unmounted ?? [],
      ...(business?.operations.length ? { business: { operations: business.operations, awaitedCalls: business.awaitedCalls, changedFunctions: business.changedFunctions, originalHash: hash(clientSource), adaptedHash: hash(business.code) } } : {}) },
    static: { summary: report.summary, degraded: report.entries.filter((item) => ['partial', 'dropped'].includes(item.status)) },
    files: { kept: kept.length, omitted: [] },
  };
  writeFileSync(join(options.outDir, 'extension/compat.json'), JSON.stringify(compatReport, null, 2) + '\n');
  return { manifest, report: compatReport };
}
