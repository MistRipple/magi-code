// 上游原包的安装封装。静态扫描先做准入；随后在受限记录器中启动一次真实
// Cordis，只记录准入证据。安装后的原包由 daemon 管理的 Node 宿主运行，
// 动态贡献仍在运行期发布，不把记录器输出当作完整业务验收。
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRegistry } from '../index.mjs';
import { packageId } from '../core/identity.mjs';
import { analyzeNativeClient, findClientFile, usesComposerClient } from './client-assets.mjs';
import { resolveUpstreamIcon } from './icon.mjs';
import { installDependencies } from './dependencies.mjs';
import { copyBrowserAssets } from './browser-assets.mjs';
import { recordUpstream } from './record.mjs';
import { summarize, verdictFor } from '../core/report.mjs';
import { classify as classifyDsh } from '../dsh/catalog.mjs';
import { clientViews } from './client-views.mjs';
import { dshEnvironment, checkDshCompatibility } from '../core/dsh-compatibility.mjs';
import { translateClientBusiness } from './client-business.mjs';
import { REGISTERED_SETTINGS_RESOURCE } from '../runtime/registered-settings.mjs';
import { backendEntry, readPackage } from '../core/package.mjs';
import { translationRuntimeId } from '../core/translation-runtime.mjs';

const COMPAT_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TRANSLATOR_VERSION = 2;
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
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
      const sourceStat = statSync(full), size = sourceStat.size; total += size;
      const limit = name.startsWith('native/') ? 64 * 1024 * 1024 : 4 * 1024 * 1024;
      if (size > limit || total > 64 * 1024 * 1024 || files.length >= 4096) throw new Error('上游原包及依赖超过有界安装限制');
      const dest = join(target, 'extension/upstream', name);
      mkdirSync(dirname(dest), { recursive: true }); copyFileSync(full, dest);
      if (name.startsWith('native/') && (sourceStat.mode & 0o111)) chmodSync(dest, 0o755);
      files.push(name);
    }
  };
  visit(source); return files;
}
function clientDocument(configFile, styles = []) {
  return `<!doctype html>
<meta charset="utf-8"><meta name="color-scheme" content="light dark">${styles.map(name => `<link rel="stylesheet" href="${name}">`).join('')}
<div id="root"></div><script src="${configFile}"></script><script src="host.js"></script>
`;
}

export async function prepareDsh(options) {
  const preparedRuntimeId = translationRuntimeId(options.sharedRoot);
  const pkg = readPackage(options.upstreamDir);
  const translator = createRegistry().create('dsh');
  if (!(await translator.detect(options.upstreamDir))) throw new Error(`${pkg.name} 不是 DSH 插件`);
  const compatibility = checkDshCompatibility(pkg, dshEnvironment(options.sharedRoot), options.compatibility);
  if (existsSync(options.outDir) && readdirSync(options.outDir).length) throw new Error('输出目录必须为空');
  const main = backendEntry(pkg), root = resolve(options.upstreamDir), entry = resolve(root, main);
  if (!entry.startsWith(root + sep) || !existsSync(entry) || !/\.(?:mjs|cjs|js)$/.test(main)) throw new Error('上游入口无效或尚未构建');
  const inspection = await translator.inspect(root);
  if (inspection.verdict.blocked.length) throw new Error(`Magi 兼容检查未通过：\n${inspection.verdict.blocked.map((item) => `${item.name}（${item.status === 'unknown' ? '尚未完成兼容核对' : '宿主能力尚未接入'}）: ${item.note}`).join('\n')}`);
  const dependencies = await installDependencies(root, options.sharedRoot, undefined, compatibility);
  // 条件注入的未实现 Session 方法继续保持服务缺席，不能因新增命名空间
  // 让原有可选降级分支突然运行。只有必需命名空间读取申请目录授权。
  const readSessionDirectory = inspection.entries.some((entry) => !entry.optional
    && (entry.kind === 'injects' && entry.name === 'sessions' || entry.kind === 'calls' && entry.name === 'ctx.sessions'));
  // 静态扫描不能证明 apply() 能启动，也看不到延迟注册的工具/事件。安装前在
  // 受限记录器中按默认配置启动一次真实 Cordis，仅保留观察证据；
  // 动态贡献由激活后的原包发布，不能从安装期快照生成静态贡献。
  const recorded = recordUpstream({ entry, sharedRoot: options.sharedRoot, upstreamDir: root, readSessionDirectory });
  const knownEvents = new Set(inspection.entries.filter(item => item.kind === 'events').map(item => item.name));
  const runtimeEntries = recorded.listeners.filter(name => !knownEvents.has(name)).map((name) => {
    const rule = classifyDsh('events', name);
    return { kind: 'events', name, category: rule.category, status: rule.status, target: rule.target, note: rule.note, source: rule.source, needs: rule.needs, capability: rule.capability, count: 1, files: [] };
  });
  // 静态条目保留原有可选语义；新增观察按同一规则分类，未知事件仍阻塞。
  const entries = [...inspection.entries, ...runtimeEntries];
  const reportSummary = summarize(entries);
  const reportVerdict = verdictFor(entries, inspection.verdict.required);
  const report = { ...inspection, entries, summary: reportSummary, verdict: reportVerdict };
  if (reportVerdict.blocked.length) throw new Error(`Magi 运行期兼容检查未通过：\n${reportVerdict.blocked.map((item) => `${item.name}（${item.status}）: ${item.note}`).join('\n')}`);
  const manifest = {
    sdkVersion: 2, id: packageId('dsh', pkg.name), version: pkg.version,
    name: pkg.displayName ?? pkg.name, description: pkg.description ?? '', dataSchemaVersion: 1,
    extension: { hooks: [], compat: { kind: 'dsh' }, runtimeContributions: ['tools', 'commands', 'modelSources', 'settings', 'skills'] },
    permissions: [
      { kind: 'sessionWrite', targets: ['context', 'wake'] },
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
  const management = inspection.entries.some((entry) => /^(?:ctx\.)?(?:pluginManager|loader)(?:\.|$)/.test(entry.name))
    || ['pluginManager', 'loader'].some((name) => Object.hasOwn(inspection.serviceReads, name));
  const packageManagement = Object.hasOwn(inspection.serviceMembers, 'profileContext.packageManager') || recorded.profilePackageManager
    || inspection.entries.some(entry => entry.name === 'ctx.profileContext.packageManager');
  const profileReload = inspection.entries.some(entry => /^ctx\.hmr(?:\.|$)/.test(entry.name) || entry.kind === 'injects' && entry.name === 'hmr')
    || Object.hasOwn(inspection.serviceReads, 'hmr');
  const profileWrites = inspection.profileMutations.length > 0;
  if (management || packageManagement || inspection.profilePackageProcesses.length > 0) {
    manifest.permissions.push({ kind: 'pluginManagement', targets: ['install', 'lifecycle', 'authorize', ...(management ? ['compatibility'] : []), 'background'] });
  } else if (profileReload || profileWrites) {
    manifest.permissions.push({ kind: 'pluginManagement', targets: ['lifecycle', 'background'] });
  }
  if (readSessionDirectory) manifest.permissions.push({ kind: 'sessionRead', targets: ['directory'] });
  if (inspection.entries.some(entry => entry.name === 'attachments' || /^ctx\.attachments(?:\.|$)/.test(entry.name)) || Object.hasOwn(inspection.serviceReads, 'attachments')) {
    manifest.permissions.push({ kind: 'attachments', targets: ['images'] });
  }
  const registeredSettings = inspection.entries.some(entry => entry.name === 'ctx.settings.register')
    || Object.hasOwn(inspection.serviceReads, 'settings') || recorded.settingsNamespaces.length > 0;
  if (registeredSettings) {
    manifest.permissions.find(permission => permission.kind === 'storage').targets.push(REGISTERED_SETTINGS_RESOURCE);
    manifest.contributions.resources = [{ id: REGISTERED_SETTINGS_RESOURCE, title: 'DSH 设置', description: '由 daemon 持久化的插件命名空间设置' }];
  }
  mkdirSync(options.outDir, { recursive: true });
  const kept = copyTree(root, options.outDir);
  const compatRuntimeId = compatibility.runtimeId;
  const optionalEvents = inspection.entries.filter(item => item.kind === 'events' && item.optional).map(item => item.name);
  writeFileSync(join(options.outDir, 'extension/index.mjs'), `import { activateDsh } from '@magi/compat-dsh';\nimport * as upstream from ${JSON.stringify(`./upstream/${main}`)};\nexport default api => activateDsh(api, upstream, ${JSON.stringify({ runtimeVersion: TRANSLATOR_VERSION, plugin: pkg.name, runtimeId: compatRuntimeId, optionalEvents, readSessionDirectory, settingsNamespaces: registeredSettings })});\n`);
  mkdirSync(join(options.outDir, 'ui'), { recursive: true });
  copyFileSync(join(COMPAT_DIR, 'dist/dsh-client-host.js'), join(options.outDir, 'ui/host.js'));
  let clientAnalysis = null;
  if (client) {
    clientAnalysis = analyzeNativeClient({ upstreamDir: root, pkg, sharedRoot: options.sharedRoot });
    const assets = copyBrowserAssets(COMPAT_DIR, join(options.outDir, 'ui'));
    writeFileSync(join(options.outDir, 'ui/client.js'), business.code);
    const views = clientViews(clientAnalysis.mounted, manifest.name);
    manifest.contributions.views = views.map(({ slot, ...view }) => view);
    if (business.operations.length) {
      manifest.contributions.client = { entry: views[0].entry };
      manifest.permissions.push({ kind: 'client', targets: ['persistent'] });
    }
    const clientViewCatalog = views.map(view => ({ id: view.id, name: view.slot.name, key: view.slot.key }));
    const panelViews = views.filter((view) => view.placements.includes('rightPane')).map((view) => ({ key: view.slot.key, viewId: view.id }));
    for (const { slot, id, entry } of views) {
      const configFile = `dsh-config-${id}.js`;
      writeFileSync(join(options.outDir, 'ui', configFile), `globalThis.__MAGI_DSH_CLIENT__ = ${JSON.stringify({ runtimeVersion: 2, clientScript: 'client.js', libraryScripts: assets.scripts, slotName: slot.name, slotKey: slot.key, clientViews: clientViewCatalog, panelViews, business: { operations: business.operations } })};\n`);
      writeFileSync(join(options.outDir, entry), clientDocument(configFile, assets.styles));
    }
  } else {
    writeFileSync(join(options.outDir, 'ui/dsh-config-settings.js'), `globalThis.__MAGI_DSH_CLIENT__ = ${JSON.stringify({ runtimeVersion: 2, settingsForm: true })};\n`);
    writeFileSync(join(options.outDir, 'ui/settings.html'), clientDocument('dsh-config-settings.js'));
  }
  const icon = await resolveUpstreamIcon({ upstreamDir: root, pkg });
  if (icon) { writeFileSync(join(options.outDir, icon.to), icon.bytes); manifest.icon = icon.to; }
  writeFileSync(join(options.outDir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  const compatReport = {
    kind: 'dsh', translatorVersion: TRANSLATOR_VERSION, runtimeId: compatRuntimeId, translationRuntimeId: preparedRuntimeId,
    upstream: { name: pkg.name, version: pkg.version, integrity: options.source.integrity, ...(options.source.github ? { github: options.source.github } : {}) },
    compatibility,
    magi: { id: manifest.id, version: manifest.version }, dependencies,
    runtimeContributions: manifest.extension.runtimeContributions,
    serviceReads: inspection.serviceReads,
    profileMutations: inspection.profileMutations,
    tools: recorded.tools, commands: recorded.commands, modelSources: recorded.providers,
    routes: recorded.routes, listeners: recorded.listeners, settingsNamespaces: recorded.settingsNamespaces,
    ui: { mode: client ? 'native' : 'schema', mountedSlots: clientAnalysis?.mounted ?? [], unmountedSlots: clientAnalysis?.unmounted ?? [],
      ...(business?.operations.length ? { business: { operations: business.operations, awaitedCalls: business.awaitedCalls, changedFunctions: business.changedFunctions, originalHash: hash(clientSource), adaptedHash: hash(business.code) } } : {}) },
    static: { summary: report.summary, degraded: report.entries.filter((item) => ['partial', 'dropped'].includes(item.status)) },
    files: { kept: kept.length, omitted: [] },
  };
  writeFileSync(join(options.outDir, 'extension/compat.json'), JSON.stringify(compatReport, null, 2) + '\n');
  return { manifest, report: compatReport };
}
