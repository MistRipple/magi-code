// DSH profile 的调用期投影。同步文件 API 读取调用开始时的 daemon 快照；
// 文件修改只形成当前调用的提案，回执前沿 plugins 管理入口提交。没有物理
// profile、文件监听器或另一份安装注册表；取消、异常与版本冲突不会提交草稿。
import { AsyncLocalStorage } from 'node:async_hooks';
import { join, resolve, relative, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { load, dump } from 'js-yaml';
import { createPluginManager } from './plugin-manager.mjs';
import { pluginLifecycle } from './plugin-directory.mjs';

const scopes = new AsyncLocalStorage();
let host;
const DESCRIPTOR_BASE = 0x40000000;
let nextDescriptor = DESCRIPTOR_BASE;
const error = (code, message, path) => Object.assign(new Error(message), { code, path });
// Atomic writers may use their own temporary or backup suffix. These files
// exist only for this invocation and never become a second installation store.
const temporaryProposal = (key) => /^(?:package\.json|cordis\.patch\.yml)[.~-][^/]{1,160}$/.test(key);
export const profilePath = () => host?.root;
export function configureProfile(api, { home = process.env.DSH_HOME, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org' } = {}) {
  if (!home || !api.plugins?.list) return () => {};
  if (host) throw new Error('DSH profile 已绑定宿主');
  if (api.plugins.background) api = { ...api, plugins: api.plugins.background };
  host = { api, registry, root: resolve(home, 'profiles', 'magi'), parent: resolve(home, 'profiles') };
  const old = { dir: process.env.DSH_PROFILE_DIR, name: process.env.DSH_PROFILE };
  process.env.DSH_PROFILE_DIR = host.root; process.env.DSH_PROFILE = 'magi';
  return () => {
    host = undefined;
    if (old.dir === undefined) delete process.env.DSH_PROFILE_DIR; else process.env.DSH_PROFILE_DIR = old.dir;
    if (old.name === undefined) delete process.env.DSH_PROFILE; else process.env.DSH_PROFILE = old.name;
  };
}

function bundle(plugin) {
  const { source } = plugin;
  if (source.kind !== 'upstream' || !source.location.startsWith('dsh:')) return;
  const spec = source.location.slice(4), index = spec.lastIndexOf('@');
  if (index <= 0) throw new Error('daemon 上游插件身份无效');
  return { name: spec.slice(0, index), version: spec.slice(index + 1), plugin };
}
async function refresh(scope) {
  const list = await host.api.plugins.list({ includeUpstreamManifest: true });
  scope.compatibility = host.api.plugins.compatibility ? await host.api.plugins.compatibility() : null;
  scope.bundles = list.plugins.map(bundle).filter(Boolean);
  scope.original = new Map(scope.bundles.map((item) => [item.name, { ...item, digest: item.plugin.digest }]));
  scope.files = new Map(); scope.dirty = new Set();
  const rows = scope.bundles.map(({ name, plugin }) => ({ id: plugin.manifest.id, name, disabled: !plugin.enabled }));
  scope.files.set('package.json', JSON.stringify({
    name: 'magi-dsh-profile', private: true,
    dsh: { profile: { bundles: scope.bundles.map(({ name }) => name) } },
    dependencies: Object.fromEntries(scope.bundles.map(({ name, version }) => [name, version])),
  }));
  scope.files.set('cordis.yml', dump(rows));
  scope.files.set('cordis.patch.yml', dump(rows.map(({ id, disabled }) => ({ id, disabled }))));
  scope.files.set('.npmrc', `registry=${host.registry}\n`);
  if (scope.compatibility) scope.files.set('compatibility.json', JSON.stringify(scope.compatibility.exemptions));
  for (const { name, version, plugin } of scope.bundles) {
    const path = `node_modules/${name}`;
    // 缺失的原包元数据保持缺失，不能从 Magi 的名称/描述拼一份假清单，
    // 否则 engines、peers、仓库和 dsh 声明都会在健康/升级报告里丢失。
    const metadata = plugin.upstreamManifest;
    if (metadata) {
      if (metadata.name !== name || metadata.version !== version) throw new Error('daemon 上游原包元数据身份无效');
      scope.files.set(`${path}/package.json`, JSON.stringify(metadata));
    }
    const declared = metadata?.dsh?.bundle?.patch ?? './cordis.patch.yml';
    const patches = Array.isArray(declared) ? declared : [declared];
    if (patches.length > 32 || patches.some((patch) => typeof patch !== 'string')) throw new Error('上游 bundle patch 声明无效');
    for (const [index, patch] of [...new Set(patches)].entries()) {
      const key = relative(path, resolve(path, patch)).split(/[\\/]/).join('/');
      if (key === '..' || key.startsWith('../') || !key || key === 'package.json') throw new Error('上游 bundle patch 路径越界或无效');
      // Magi 的安装/启停单位是整个译包，由一个 daemon 监管的根实例承载。
      // 多层 bundle 声明仍只投影一次这个真实根行，不制造重复运行实例。
      scope.files.set(`${path}/${key}`, dump(index === 0 ? [{ insert: [{ id: plugin.manifest.id, name }] }] : []));
    }
  }
}

export function profileCompatibility() {
  const value = state().compatibility;
  if (!value) throw error('ENOTSUP', 'daemon 未提供版本兼容策略');
  return structuredClone(value);
}
export async function updateProfileExemption(target, runtimeVersion, enabled, acceptRisk) {
  const scope = state(), current = profileCompatibility();
  const value = await host.api.plugins.setVersionExemption({ target, runtimeVersion, enabled, acceptRisk,
    expectedRuntimeId: current.runtimeId, expectedRevision: current.revision });
  scope.compatibility = value;
  scope.files.set('compatibility.json', JSON.stringify(value.exemptions));
  return structuredClone(value);
}

function state() {
  const scope = scopes.getStore();
  if (!scope) throw error('EPERM', 'DSH profile 只能在 daemon 已签发的调用快照内访问');
  scope.signal?.throwIfAborted();
  return scope;
}
export function virtualPath(input) {
  if (!host || (typeof input !== 'string' && !Buffer.isBuffer(input) && !(input instanceof URL))) return;
  const path = resolve(input instanceof URL ? fileURLToPath(input) : String(input));
  if (path === host.parent) return { path, key: '..' };
  const key = relative(host.root, path).split(/[\\/]/).join('/');
  if (key === '..' || key.startsWith('../') || resolve(host.root, key) !== path) return;
  return { path, key };
}
function directory(scope, key) { return key === '..' || key === '' || [...scope.files.keys()].some((path) => path.startsWith(`${key}/`)); }
export function profileRead(path, encoding) {
  const location = virtualPath(path); if (!location) return;
  const scope = state(), value = scope.files.get(location.key);
  if (value === undefined) throw error(directory(scope, location.key) ? 'EISDIR' : 'ENOENT', `profile 文件不存在：${path}`, path);
  const data = Buffer.from(value);
  const format = typeof encoding === 'string' ? encoding : encoding?.encoding;
  return format ? data.toString(format) : data;
}

// 保留的高位描述符只指向本次调用快照，不持有 OS 文件句柄。关闭或调用结束后
// 立即失效；带 position 的读取不推进游标，语义与 Node 只读 fd 一致。
export const isProfileDescriptor = (fd) => Number.isInteger(fd) && fd >= DESCRIPTOR_BASE;
function descriptor(fd) {
  const scope = state(), value = scope.descriptors.get(fd);
  if (!value) throw error('EBADF', 'profile 文件描述符已关闭或不属于当前调用');
  return value;
}
export function profileOpen(path, flags = 'r') {
  const location = virtualPath(path); if (!location) return;
  if (flags !== 'r' && flags !== 0) throw error('EROFS', 'profile 描述符只允许读取快照', path);
  const data = profileRead(path), stat = profileStat(path), scope = state();
  if (scope.descriptors.size >= 256) throw error('EMFILE', 'profile 调用内的文件描述符过多');
  const fd = nextDescriptor++;
  scope.descriptors.set(fd, { data, stat, position: 0 });
  return fd;
}
export function profileFstat(fd) { return { ...descriptor(fd).stat }; }
export function profileClose(fd) { descriptor(fd); state().descriptors.delete(fd); }
export function profileReadRemaining(fd, options) {
  const file = descriptor(fd), data = Buffer.from(file.data.subarray(file.position));
  file.position = file.data.length;
  const encoding = typeof options === 'string' ? options : options?.encoding;
  return encoding ? data.toString(encoding) : data;
}
export function profileReadDescriptor(fd, buffer, offset = 0, length = buffer.byteLength - offset, position = null) {
  const file = descriptor(fd);
  if (!ArrayBuffer.isView(buffer)) throw new TypeError('buffer 必须是 Buffer 或 TypedArray');
  if (!Number.isInteger(offset) || !Number.isInteger(length) || offset < 0 || length < 0 || offset + length > buffer.byteLength) throw new RangeError('读取范围无效');
  if (position !== null && (!Number.isSafeInteger(position) || position < 0)) throw new RangeError('读取位置无效');
  const start = position ?? file.position;
  const count = Math.min(length, Math.max(0, file.data.length - start));
  const target = Buffer.from(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  if (count) file.data.copy(target, offset, start, start + count);
  if (position === null) file.position += count;
  return count;
}
export function profileWrite(path, data, encoding) {
  const location = virtualPath(path); if (!location) return false;
  const scope = state();
  if (location.key !== 'package.json' && location.key !== 'cordis.patch.yml' && !temporaryProposal(location.key)) throw error('EROFS', `profile 投影只允许提交清单与启停提案：${path}`, path);
  const text = Buffer.isBuffer(data) ? data.toString(typeof encoding === 'string' ? encoding : encoding?.encoding ?? 'utf8') : String(data);
  if (Buffer.byteLength(text) > 1024 * 1024) throw error('EFBIG', 'profile 提案超过 1 MiB');
  if (!scope.files.has(location.key) && scope.dirty.size >= 256) throw error('EFBIG', 'profile 调用内的提案文件过多');
  scope.files.set(location.key, text); scope.dirty.add(location.key); return true;
}
export function profileRemove(path) {
  const location = virtualPath(path); if (!location) return false;
  if (!temporaryProposal(location.key)) throw error('EROFS', '只能移除 profile 调用内的临时提案文件');
  const scope = state(); scope.files.delete(location.key); scope.dirty.delete(location.key); return true;
}
export function profileRename(from, to) {
  const source = virtualPath(from), target = virtualPath(to);
  if (!source && !target) return false;
  if (!source || !target) throw error('EXDEV', '不能在 profile 投影与物理文件之间移动数据');
  const data = profileRead(from); profileWrite(to, data); profileRemove(from); return true;
}
export function profileStat(path) {
  const location = virtualPath(path); if (!location) return;
  const scope = state(), file = scope.files.get(location.key), dir = directory(scope, location.key);
  if (file === undefined && !dir) throw error('ENOENT', `profile 路径不存在：${path}`, path);
  return { size: file === undefined ? 0 : Buffer.byteLength(file), mode: dir ? 0o40555 : 0o100444, mtimeMs: scope.time, mtime: new Date(scope.time),
    isFile: () => !dir, isDirectory: () => dir, isSymbolicLink: () => false, isBlockDevice: () => false, isCharacterDevice: () => false, isFIFO: () => false, isSocket: () => false };
}
export function profileList(path, options) {
  const location = virtualPath(path); if (!location) return;
  const scope = state();
  if (!directory(scope, location.key)) throw error('ENOENT', `profile 目录不存在：${path}`, path);
  const names = location.key === '..' ? ['magi'] : [...new Set([...scope.files.keys()].filter((key) => !location.key || key.startsWith(location.key + '/')).map((key) => key.slice(location.key ? location.key.length + 1 : 0).split('/')[0]))].sort();
  return names.map((name) => options?.withFileTypes ? { name, parentPath: location.path, path: location.path, ...profileStat(join(location.path, name)) } : options?.encoding === 'buffer' ? Buffer.from(name) : name);
}
export function loaderEntries() {
  const scope = state();
  return [ { options: { id: 'magi-profile', name: 'cordis:include', group: true, config: { path: join(host.root, 'cordis.yml') } } },
    ...scope.bundles.map(({ name, plugin }) => ({ disabled: !plugin.enabled, options: { id: `magi-profile:${plugin.manifest.id}`, name, disabled: !plugin.enabled },
      fiber: pluginLifecycle(plugin) })) ];
}

async function commit(scope) {
  scope.signal?.throwIfAborted();
  if (!scope.dirty.size) return;
  const latest = await host.api.plugins.list();
  for (const current of latest.plugins) {
    const original = scope.original.get(bundle(current)?.name);
    if (original && current.digest !== original.digest) throw error('ECONFLICT', '插件版本在 profile 提案期间已变化，请刷新后重试');
  }
  if (scope.dirty.has('package.json')) {
    const proposed = JSON.parse(scope.files.get('package.json'));
    const expected = Object.fromEntries(scope.bundles.map(({ name, version }) => [name, version]));
    // 删除依赖后再安装的上游事务可在调用内执行；最终清单必须与真正安装
    // 结果一致。不能让单纯写 package.json 成为绕开原包完整性验证的安装入口。
    if (JSON.stringify(Object.entries(proposed.dependencies ?? {}).sort()) !== JSON.stringify(Object.entries(expected).sort())) throw error('EINVAL', 'profile 清单提案与 daemon 安装事实不一致；安装或移除必须经过插件管理入口');
  }
  if (!scope.dirty.has('cordis.patch.yml')) return;
  const rows = load(scope.files.get('cordis.patch.yml'));
  if (!Array.isArray(rows)) throw error('EINVAL', 'loader 启停提案必须是数组');
  const byId = new Map(scope.bundles.map((item) => [item.plugin.manifest.id, item]));
  const pending = new Map();
  for (const row of rows) {
    if (!row || Object.keys(row).some((key) => !['id', 'disabled'].includes(key)) || typeof row.disabled !== 'boolean' || !byId.has(row.id)) throw error('EINVAL', 'loader 提案只能修改当前已安装插件的 disabled 状态');
    pending.set(row.id, row.disabled);
  }
  for (const [id, disabled] of pending) {
    const current = byId.get(id);
    if (current.plugin.enabled !== !disabled) {
      scope.signal?.throwIfAborted();
      await host.api.plugins.setEnabled(id, !disabled, current.plugin.digest);
    }
  }
}
export async function withProfile(callback, signal) {
  if (!host || scopes.getStore()) return callback();
  const scope = { signal, time: Date.now(), descriptors: new Map() };
  await refresh(scope);
  return scopes.run(scope, async () => {
    try { const result = await callback(); if (!(result?.status >= 400 || result?.body?.ok === false || result?.isError)) await commit(scope); return result; }
    finally { scope.descriptors.clear(); }
  });
}

// 只接管明确针对当前虚拟 profile 的 dsh plugin 命令。其他进程仍走原生
// Node；这里不提供 Magi CLI，也不在 PATH 放置替身程序。
export function profileSpawn(file, args = []) {
  if (!host || !/^(dsh|dsh\.cmd)$/.test(basename(String(file)))) return;
  const child = new EventEmitter(); child.stdout = new PassThrough(); child.stderr = new PassThrough(); child.stdin = new PassThrough(); child.pid = undefined;
  const controller = new AbortController(); child.kill = () => { controller.abort(); return true; };
  queueMicrotask(async () => {
    let code = 1;
    try {
      const scope = state(); scope.signal?.throwIfAborted();
      const argv = args.map(String);
      if (argv[0] !== 'plugin') throw new Error('仅翻译 dsh plugin 管理命令');
      const p = argv.indexOf('--profile');
      if (p >= 0) { if (argv[p + 1] !== 'magi') throw new Error('不能操作其他 DSH profile'); argv.splice(p, 2); }
      const [_, verb, spec, ...rest] = argv;
      if (rest.length || !spec || !['add', 'remove'].includes(verb)) throw new Error('DSH 管理命令参数不受支持');
      controller.signal.throwIfAborted();
      const manager = createPluginManager(host.api, host.registry);
      const result = verb === 'add' ? await manager.installBundle(spec, { enabled: true }) : await manager.removeBundle(spec);
      if (result.application === 'failed') throw new Error(result.error?.message ?? '插件管理失败');
      await refresh(scope);
      child.stdout.end('Magi daemon 已提交插件管理操作\n'); code = 0;
    } catch (cause) { child.stderr.end(String(cause.message ?? cause)); }
    finally { child.stdout.end(); child.stderr.end(); child.emit('exit', code, null); child.emit('close', code, null); }
  });
  return child;
}

