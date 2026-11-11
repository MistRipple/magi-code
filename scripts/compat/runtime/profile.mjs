// DSH profile 的调用期投影。同步文件 API 读取调用开始时的 daemon 快照；
// 文件修改只形成当前调用的提案，回执前沿 plugins 管理入口提交。没有物理
// profile、文件监听器或另一份安装注册表；取消、异常与版本冲突不会提交草稿。
import { AsyncLocalStorage } from 'node:async_hooks';
import { join, resolve, relative, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { load, dump } from 'js-yaml';
import { createPluginManager } from './plugin-manager.mjs';
import { pluginLifecycle } from './plugin-directory.mjs';
import { dshBundle as bundle } from './plugin-source.mjs';
import { createProfileProcesses, packageManagerScript, profilePackageArgs, profilePackageCommand } from './profile-process-host.mjs';
import { readFileSync, statSync, readdirSync, realpathSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { unzipSync } from 'fflate';

const scopes = new AsyncLocalStorage();
let host;
const DESCRIPTOR_BASE = 0x40000000;
let nextDescriptor = DESCRIPTOR_BASE;
const error = (code, message, path) => Object.assign(new Error(message), { code, path });
// Atomic writers may use their own temporary or backup suffix. These files
// exist only for this invocation and never become a second installation store.
const temporaryProposal = (key) => /^(?:package\.json|cordis\.patch\.yml)[.~-][^/]{1,160}$/.test(key);
export const profilePath = () => host?.root;
export function profileChangeId(create = false) {
  if (!host) return;
  const scope = state();
  if (create) scope.changeId ??= crypto.randomUUID();
  return scope.changeId;
}
export function configureProfile(api, { home = process.env.DSH_HOME, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org' } = {}) {
  if (!home || !api.plugins?.list) return () => {};
  if (host) throw new Error('DSH profile 已绑定宿主');
  if (api.plugins.background) api = { ...api, plugins: api.plugins.background };
  const canonicalHome = realpathSync(home);
  host = { api, registry, root: resolve(home, 'profiles', 'magi'), parent: resolve(home, 'profiles'),
    canonicalRoot: resolve(canonicalHome, 'profiles', 'magi'), canonicalParent: resolve(canonicalHome, 'profiles') };
  host.processes = createProfileProcesses(api, { home, registry });
  const old = { dir: process.env.DSH_PROFILE_DIR, name: process.env.DSH_PROFILE };
  process.env.DSH_PROFILE_DIR = host.root; process.env.DSH_PROFILE = 'magi';
  return () => {
    host.processes.dispose();
    host = undefined;
    if (old.dir === undefined) delete process.env.DSH_PROFILE_DIR; else process.env.DSH_PROFILE_DIR = old.dir;
    if (old.name === undefined) delete process.env.DSH_PROFILE; else process.env.DSH_PROFILE = old.name;
  };
}

async function refresh(scope) {
  const { api } = state(scope).owner;
  const list = await api.plugins.list({ includeUpstreamManifest: true });
  state(scope);
  const compatibility = api.plugins.compatibility ? await api.plugins.compatibility() : null;
  state(scope);
  applyProfileSnapshot(scope, { list, compatibility });
}
export function applyProfileSnapshot(scope, { list, compatibility }) {
  scope = state(scope);
  scope.compatibility = compatibility;
  scope.bundles = list.plugins.map(bundle).filter(Boolean);
  scope.original = new Map(scope.bundles.map((item) => [item.name, { ...item, digest: item.plugin.digest }]));
  scope.files = new Map(); scope.dirty = new Set();
  scope.archives = new Map();
  const rows = scope.bundles.map(({ name, plugin }) => ({ id: plugin.manifest.id, name, disabled: !plugin.enabled }));
  scope.files.set('package.json', JSON.stringify({
    name: 'magi-dsh-profile', private: true,
    dsh: { profile: { bundles: scope.bundles.map(({ name }) => name) } },
    dependencies: Object.fromEntries(scope.bundles.map(({ name, source }) => [name, source])),
  }));
  scope.files.set('cordis.yml', dump(rows));
  scope.files.set('cordis.patch.yml', dump(rows.map((row, index) => ({ ...row,
    ...(scope.bundles[index].plugin.disabledBy ? { comment: scope.bundles[index].plugin.disabledBy.reason } : {}),
  }))));
  scope.files.set('.npmrc', `registry=${host.registry}\n`);
  if (scope.compatibility) scope.files.set('compatibility.json', JSON.stringify(scope.compatibility.exemptions));
  for (const { name, version, plugin } of scope.bundles) {
    const path = `node_modules/${name}`;
    if (plugin.upstreamArchivePath) scope.archives.set(path, { path: plugin.upstreamArchivePath, digest: plugin.digest });
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

// 原包文件从当前安装归档按调用懒读取；不生成 node_modules 副本，不依赖
// 激活时才物化的运行目录。摘要变化只能由下一份 daemon 快照接纳。
function readArtifact(scope, key) {
  for (const [root, archive] of scope.archives) {
    if (key !== root && !key.startsWith(root + '/')) continue;
    if (!archive.stat) {
      const bytes = readFileSync(archive.path);
      if (createHash('sha256').update(bytes).digest('hex') !== archive.digest) throw error('ECONFLICT', '已安装归档与 daemon 摘要不一致，请重新读取插件目录');
      const stat = statSync(archive.path);
      const prefix = 'extension/upstream/';
      const files = unzipSync(bytes, { filter: file => file.name.startsWith(prefix) });
      for (const [name, data] of Object.entries(files)) {
        if (name.endsWith('/')) continue;
        const path = `${root}/${name.slice(prefix.length)}`;
        if (!scope.files.has(path)) scope.files.set(path, Buffer.from(data));
      }
      archive.stat = stat;
    }
    return archive.stat;
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

function state(scope = scopes.getStore()) {
  if (!scope || scope.closed || !scope.owner || scope.owner !== host) throw error('EPERM', 'DSH profile 只能在 daemon 已签发的当前实例调用快照内访问');
  scope.signal?.throwIfAborted();
  return scope;
}
export function virtualPath(input) {
  if (!host || (typeof input !== 'string' && !Buffer.isBuffer(input) && !(input instanceof URL))) return;
  const path = resolve(input instanceof URL ? fileURLToPath(input) : String(input));
  if (path === host.parent || path === host.canonicalParent) return { path: host.parent, key: '..' };
  let key;
  for (const root of [host.root, host.canonicalRoot]) {
    const candidate = relative(root, path).split(/[\\/]/).join('/');
    if (candidate !== '..' && !candidate.startsWith('../') && resolve(root, candidate) === path) { key = candidate; break; }
  }
  if (key === undefined) return;
  // 安装元数据和启停配置仍完全来自 daemon。其余路径是当前插件的私有
  // 业务文件（报告、导出、缓存），沿真实 Node 文件系统保存和返回实际路径。
  // 不把这些文件当安装事实，也不生成 package.json 或 node_modules 副本。
  const top = key.split('/')[0];
  if (key && !['node_modules', 'package.json', 'cordis.yml', 'cordis.patch.yml', '.npmrc', 'compatibility.json'].includes(top) && !temporaryProposal(top)) return;
  return { path: resolve(host.root, key), key };
}
export function profileRealpath(path) {
  const location = virtualPath(path); if (!location) return;
  profileStat(path);
  return location.key === '..' ? host.canonicalParent : resolve(host.canonicalRoot, location.key);
}
function directory(scope, key) { return key === '..' || key === '' || [...scope.files.keys()].some((path) => path.startsWith(`${key}/`)); }
export function profileRead(path, encoding) {
  const location = virtualPath(path); if (!location) return;
  const scope = state(); readArtifact(scope, location.key);
  const value = scope.files.get(location.key);
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
  const scope = state(), artifact = readArtifact(scope, location.key), file = scope.files.get(location.key), dir = directory(scope, location.key);
  if (file === undefined && !dir) throw error('ENOENT', `profile 路径不存在：${path}`, path);
  const time = artifact?.mtimeMs ?? scope.time;
  return { size: file === undefined ? 0 : Buffer.byteLength(file), mode: dir ? 0o40555 : 0o100444, mtimeMs: time, mtime: new Date(time),
    isFile: () => !dir, isDirectory: () => dir, isSymbolicLink: () => false, isBlockDevice: () => false, isCharacterDevice: () => false, isFIFO: () => false, isSocket: () => false };
}
export function profileList(path, options) {
  const location = virtualPath(path); if (!location) return;
  const scope = state(); readArtifact(scope, location.key);
  if (!directory(scope, location.key)) throw error('ENOENT', `profile 目录不存在：${path}`, path);
  const names = location.key === '..' ? ['magi'] : [...new Set([...scope.files.keys()].filter((key) => !location.key || key.startsWith(location.key + '/')).map((key) => key.slice(location.key ? location.key.length + 1 : 0).split('/')[0]))];
  if (location.key === '') {
    try { for (const name of readdirSync(host.root)) if (!virtualPath(join(host.root, name)) && !names.includes(name)) names.push(name); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  return names.sort().map(name => {
    if (!options?.withFileTypes) return options?.encoding === 'buffer' ? Buffer.from(name) : name;
    const stat = profileStat(join(location.path, name)) ?? statSync(join(location.path, name));
    return { name, parentPath: location.path, path: location.path,
      ...Object.fromEntries(['isFile', 'isDirectory', 'isSymbolicLink', 'isBlockDevice', 'isCharacterDevice', 'isFIFO', 'isSocket'].map(method => [method, () => stat[method]()])),
    };
  });
}
export function loaderEntries() {
  const scope = state();
  return scope.bundles.map(({ name, plugin }) => ({ disabled: !plugin.enabled,
    options: { id: plugin.manifest.id, name, disabled: !plugin.enabled }, fiber: pluginLifecycle(plugin) }));
}

// 原包的“应用当前 profile”操作沿 daemon 唯一监管器完成。安装归档不可变，
// 版本替换已由安装事务提交；这里不会观察物理目录或重启未变化的模块。
export function createProfileReload() {
  const owner = host, transactions = new AsyncLocalStorage();
  let queue = Promise.resolve();
  const admitted = () => {
    if (!owner || host !== owner) throw new Error('profile 重载所属实例已失效');
    return state();
  };
  return Object.freeze({
    runExclusive(callback) {
      if (transactions.getStore()?.active) return Promise.reject(new Error('HMR transactions cannot be nested'));
      const operation = queue.then(async () => {
        admitted();
        const transaction = { active: true };
        try { return await transactions.run(transaction, callback); }
        finally { transaction.active = false; }
      });
      queue = operation.catch(() => {});
      return operation;
    },
    async reconcileProfile(requiredIds = []) {
      if (!transactions.getStore()?.active) throw new Error('HMR profile reconciliation must run inside runExclusive');
      const scope = admitted();
      if (!Array.isArray(requiredIds) || requiredIds.length > 512 || requiredIds.some(id => typeof id !== 'string')) throw new Error('profile 必需条目身份无效');
      await commit(scope);
      await refresh(scope);
      admitted();
      await owner.api.plugins.reconcile({ pluginIds: scope.bundles.map(({ plugin }) => plugin.manifest.id) });
      admitted();
      await refresh(scope);
      const entries = new Map(loaderEntries().map(entry => [entry.options.id, entry]));
      for (const id of requiredIds) if (entries.get(id)?.fiber?.state !== 2) throw new Error(`profile 必需条目未运行：${id}`);
      const warnings = scope.bundles.filter(({ plugin }) => plugin.enabled && !plugin.active)
        .map(({ name }) => `${name} 尚未激活，请在 Magi 插件管理检查授权与运行状态`);
      return { warnings, pending: [] };
    },
  });
}

async function commit(scope) {
  const { api } = state(scope).owner;
  if (!scope.dirty.size) return;
  const latest = await api.plugins.list();
  state(scope);
  for (const current of latest.plugins) {
    const original = scope.original.get(bundle(current)?.name);
    if (original && current.digest !== original.digest) throw error('ECONFLICT', '插件版本在 profile 提案期间已变化，请刷新后重试');
  }
  if (scope.dirty.has('package.json')) {
    const proposed = JSON.parse(scope.files.get('package.json'));
    const expected = Object.fromEntries(scope.bundles.map(({ name, source }) => [name, source]));
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
    if (!row || Object.keys(row).some((key) => !['id', 'name', 'disabled', 'comment'].includes(key)) || typeof row.disabled !== 'boolean'
      || row.comment !== undefined && (typeof row.comment !== 'string' || Buffer.byteLength(row.comment) > 4096)
      || row.name !== undefined && typeof row.name !== 'string') throw error('EINVAL', 'loader 提案只能修改当前已安装插件的 disabled 状态及停用原因');
    const matches = row.id === undefined ? scope.bundles.filter(item => item.name === row.name) : [byId.get(row.id)].filter(Boolean);
    if (matches.length !== 1 || row.name !== undefined && row.name !== matches[0].name) throw error('EINVAL', 'loader 提案必须指向唯一的当前已安装插件');
    pending.set(matches[0].plugin.manifest.id, { disabled: row.disabled, reason: row.comment });
  }
  // 删除快照中的停用项表示恢复启用，不依赖可选的停用说明。
  // daemon 提交时继续核对预期状态、停用来源及摘要。
  for (const [id, item] of byId) if (!item.plugin.enabled && !pending.has(id)) {
    pending.set(id, { disabled: false });
  }
  for (const [id, { disabled, reason }] of pending) {
    const current = byId.get(id);
    if (current.plugin.enabled !== !disabled) {
      state(scope);
      await api.plugins.setEnabled(id, !disabled, current.plugin.digest, {
        expectedState: { enabled: current.plugin.enabled, ...(current.plugin.disabledBy ? { disabledBy: current.plugin.disabledBy } : {}) },
        ...(disabled && reason?.trim() ? { reason } : {}),
      });
    }
  }
}

// 管理服务与辅助进程在同一调用中先结算已有配置提案，再执行真实修改并重读
// daemon。上游随后读取 profile/loader 时须看到刚完成的安装，不能保留旧版本。
export function createProfileMutation(background = false, signal) {
  const owner = host;
  return callback => {
    // 命令自己的取消只约束其业务，不取消同一请求中其他命令或配置提案。
    const apply = () => { signal?.throwIfAborted(); return callback(); };
    if (!owner) return apply();
    if (host !== owner) return Promise.reject(new Error('profile 修改所属实例已失效'));
    const scope = scopes.getStore();
    if (scope && !scope.closed && !scope.signal?.aborted) return mutateProfile(apply, background);
    if (!background) return Promise.reject(new Error('profile 修改缺少活动调用快照'));
    // HTTP 202 后的工作只使用显式后台管理端口；重新读取实例事实，
    // 不继承已经关闭的请求快照、取消信号或待提交提案。
    return scopes.run(undefined, () => withProfile(() => mutateProfile(apply)));
  };
}

function mutateProfile(callback, background = false) {
  const scope = scopes.getStore(), owner = host;
  if (!scope) return callback();
  const operation = (scope.mutations ?? Promise.resolve()).then(async () => {
    if (!owner || host !== owner) throw new Error('profile 修改所属实例已失效');
    if (background && (scope.closed || scope.signal?.aborted)) {
      return scopes.run(undefined, () => withProfile(() => mutateProfile(callback)));
    }
    state(); scope.signal?.throwIfAborted();
    await commit(scope);
    if (scope.dirty.size) await refresh(scope);
    scope.signal?.throwIfAborted();
    let result, failure;
    try { result = await callback(); } catch (cause) { failure = cause; }
    if (background && (scope.closed || scope.signal?.aborted)) {
      if (failure) throw failure;
      return result;
    }
    try {
      if (host !== owner) throw new Error('profile 修改所属实例已失效');
      state(); await refresh(scope);
    } catch (cause) {
      throw Object.assign(new Error(`插件管理操作已返回，但最新目录尚未确认；请刷新状态，不要重复已提交的业务：${failure?.message ?? cause.message}`, { cause }),
        { code: 'profile-refresh-failed', receipt: result });
    }
    if (failure) throw failure;
    return result;
  });
  scope.mutations = operation.catch(() => {});
  return operation;
}
export async function withProfile(callback, signal) {
  if (!host || scopes.getStore()) return callback();
  const scope = { owner: host, signal, time: Date.now(), descriptors: new Map() };
  await refresh(scope);
  return scopes.run(scope, async () => {
    let result, failure;
    const failed = () => failure || result?.status >= 400 || result?.body?.ok === false || result?.isError || result?.application === 'failed' || result?.application === 'cancelled';
    try {
      try { result = await callback(); if (!failed()) await commit(scope); } catch (cause) { failure = cause; }
      if (scope.changeId) {
        await scope.mutations;
        try {
          const receipt = await scope.owner.api.plugins.finishChange({ changeId: scope.changeId, commit: !failed() });
          if (receipt.restored.length) {
            const notice = '未提交的更新已结束，原安装已恢复；可检查当前状态后重试。';
            if (failure) failure = Object.assign(new Error(`${failure.message}\n${notice}`, { cause: failure }), { code: failure.code });
            else if (typeof result?.body?.error === 'string') result.body.error += `\n${notice}`;
            else if (result?.error) result.error = { ...result.error, message: `${result.error.message}\n${notice}`, diagnostic: `${result.error.diagnostic ?? result.error.message}\n${notice}` };
          }
        }
        catch (cause) { throw new Error(`管理移除的结算尚未确认，请检查 Magi 插件状态，不要重复已提交的业务：${cause.message}`, { cause: failure ?? cause }); }
      }
      if (failure) throw failure;
      return result;
    }
    finally { scope.closed = true; scope.descriptors.clear(); }
  });
}

// 辅助进程的同步退出也必须先结算提案；终态回执由父实例经同一 commit 入口取得。
export async function runProfileProcess(callback, finalize) {
  const scope = { owner: host, time: Date.now(), descriptors: new Map() };
  await refresh(scope);
  return scopes.run(scope, () => {
    process.once('exit', code => finalize({
      files: code === 0 ? [...scope.dirty].filter(key => !temporaryProposal(key)).map(key => [key, scope.files.get(key)]) : [],
      expected: [...scope.original].map(([name, value]) => [name, value.digest]),
    }));
    return callback();
  });
}
export function commitProcessProposal({ files, expected }) {
  const scope = state();
  if (!Array.isArray(files) || files.length > 256 || !Array.isArray(expected)) throw new Error('辅助进程 profile 提案无效');
  if (JSON.stringify(expected) !== JSON.stringify([...scope.original].map(([name, value]) => [name, value.digest]))) throw new Error('辅助进程读取后安装目录已变化，请重试');
  for (const [key, value] of files) {
    if (!['package.json', 'cordis.patch.yml'].includes(key) || typeof value !== 'string') throw new Error('辅助进程只能提交 profile 清单和启停提案');
    profileWrite(join(host.root, key), value);
  }
}

// 只有当前 Magi 数据目录的进程才进入 profile 委托。上游业务可以明确
// 访问另一个 DSH 实例；该实例的文件、CLI 和安装事实仍归它自身拥有。
// 不提供 Magi CLI，也不在 PATH 放置替身程序。
export function profileSpawn(file, args = [], options) {
  let packageCommand;
  if (host) {
    try { packageCommand = profilePackageCommand(file, args, options, [host.root, host.canonicalRoot]); }
    catch (cause) { packageCommand = { error: cause }; }
  }
  if (host && !packageCommand) {
    const environment = options?.env ?? process.env;
    if (environment.DSH_HOME !== undefined) {
      let home;
      try { home = realpathSync(resolve(String(environment.DSH_HOME))); }
      catch (cause) { if (cause.code === 'ENOENT') return; throw cause; }
      if (home !== resolve(host.canonicalParent, '..')) return;
    } else if (options?.env != null) return;
  }
  if (host && !packageCommand && scopes.getStore()) {
    const child = host.processes.spawn(file, args, options, state().signal);
    if (child) return child;
  }
  if (!host || !packageCommand && !/^(dsh|dsh\.cmd)$/.test(basename(String(file)))) return;
  let command = packageCommand;
  if (!command) {
    try {
      const argv = args.map(String);
      if (argv[0] !== 'plugin') throw new Error('仅翻译 dsh plugin 管理命令');
      const p = argv.indexOf('--profile');
      if (p >= 0) { if (argv[p + 1] !== 'magi') throw new Error('不能操作其他 DSH profile'); argv.splice(p, 2); }
      command = profilePackageArgs(argv.slice(1));
    } catch (error) { command = { error }; }
  }
  const owner = host;
  // 管理命令复用受管辅助进程，保留真实 PID、信号和 stdio；业务仍在原调用
  // 的 profile/恢复事务内执行，不能把子进程变成另一条安装入口。
  const arguments_ = command.error ? [] : [command.action, command.spec, ...(command.reporter ? [`--reporter=${command.reporter}`] : [])];
  return owner.processes.spawn(process.execPath, [packageManagerScript, ...arguments_], options, scopes.getStore()?.signal, async (signal, installBundle) => {
    state(); signal.throwIfAborted();
    if (command.error) throw command.error;
    const manager = createPluginManager({ ...owner.api, plugins: { ...owner.api.plugins, installBundle } }, owner.registry, signal);
    return command.action === 'add'
      ? manager.installBundle(command.spec, { enabled: true, requestId: crypto.randomUUID() })
      : manager.removeBundle(command.spec);
  });
}

