// 每个扩展实例持有辅助进程的委托；请求仍经原 SDK 校验当前摘要、授权和实例租约。
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { randomBytes } from 'node:crypto';
import { mkdtempSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { MAX_PROCESS_FRAME } from './profile-process-call.mjs';
import { createPluginManager } from './plugin-manager.mjs';
import { AsyncResource } from 'node:async_hooks';
import { withProfile, commitProcessProposal } from './profile.mjs';

export const packageManagerScript = fileURLToPath(new URL('profile-package-manager.mjs', import.meta.url));
export const profilePackageManager = () => Object.freeze({ command: process.execPath, args: [packageManagerScript] });

function directoryIdentity(path) {
  try { return realpathSync(path); }
  catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const parent = dirname(path);
    return parent === path ? path : resolve(directoryIdentity(parent), basename(path));
  }
}

// npm/pnpm 只把当前 profile 的明确包操作转换为 daemon 请求；外部目录
// 继续由真实程序拥有。不会在投影目录安装依赖、生成锁文件或运行脚本。
export function profilePackageCommand(file, args, options, roots) {
  let name = basename(String(file)).replace(/\.cmd$/i, '').toLowerCase();
  if (file === process.execPath && Array.isArray(args) && /^(?:npm-cli\.js|pnpm\.(?:c?js|mjs))$/.test(basename(args[0] ?? ''))) {
    name = basename(args[0]).startsWith('npm') ? 'npm' : 'pnpm'; args = args.slice(1);
  }
  if (!['npm', 'pnpm'].includes(name) || !Array.isArray(args)) return;
  const cwd = resolve(options?.cwd instanceof URL ? fileURLToPath(options.cwd) : options?.cwd ?? process.cwd());
  const identities = new Set(roots.map(root => directoryIdentity(resolve(root))));
  const inProfile = directory => identities.has(directoryIdentity(directory));
  let directory = cwd, explicitDirectory = false, invalidDirectory = false, touchesProfile = inProfile(cwd);
  const remaining = [];
  for (let index = 0; index < args.length; index++) {
    const arg = String(args[index]);
    const flag = arg.match(/^(--prefix|--dir|-C)(?:=(.*))?$/);
    if (!flag) { remaining.push(arg); continue; }
    const value = flag[2] ?? args[++index];
    if (explicitDirectory || typeof value !== 'string' || !value || value.startsWith('-')) invalidDirectory = true;
    if (typeof value === 'string' && value && !value.startsWith('-')) {
      directory = resolve(cwd, value); touchesProfile ||= inProfile(directory);
    }
    explicitDirectory = true;
  }
  if (invalidDirectory && touchesProfile) throw new Error('包管理目录参数无效或重复');
  if (!inProfile(directory)) return;
  const [verb, spec, ...flags] = remaining;
  const action = ['add', 'install', 'i'].includes(verb) ? 'add'
    : ['remove', 'uninstall', 'rm', 'un'].includes(verb) ? 'remove' : null;
  if (!action || !spec || spec.startsWith('-')) throw new Error('Magi profile 包管理需要明确的单个安装或移除目标');
  const allowed = new Set(['--ignore-scripts', '--no-audit', '--no-fund', '--no-progress', '--yes', '--force', '-w', '--reporter=append-only']);
  if (flags.some(flag => !allowed.has(flag))) throw new Error('包管理参数未提供对应产品能力');
  return { action, spec };
}

function installSpec(args, profile) {
  const command = profilePackageCommand('pnpm', args, { cwd: profile }, [profile]);
  if (!command) throw new Error('不能操作其他 profile');
  if (command.action !== 'add') throw new Error('仅支持当前 profile 的在线 add 请求');
  return command.spec;
}

export function createProfileProcesses(api, { home, registry }) {
  const owners = new Map(), sockets = new Set();
  // 在实例初始化时捕获边界；私有连接不能继承首次打开页面的调用租约。
  const boundary = new AsyncResource('MagiProfileProcess');
  let server, directory, endpoint, closed = false;
  const snapshot = async () => ({ list: await api.plugins.list({ includeUpstreamManifest: true }),
    compatibility: api.plugins.compatibility ? await api.plugins.compatibility() : null });
  function revoke(owner) {
    if (!owners.delete(owner.token)) return;
    owner.controller.abort(new Error('辅助进程所属实例已失效'));
    for (const id of owner.installations) void api.plugins.cancelInstall(id).catch(() => {});
    for (const socket of owner.sockets) socket.destroy();
  }
  async function request(owner, method, input) {
    // 每次调用都重新准入，子进程不持有可在父实例失效后使用的快照服务。
    if (closed || !owners.has(owner.token)) throw new Error('辅助进程所属实例已失效');
    if (method === 'list') return api.plugins.list(input);
    if (method === 'compatibility') return api.plugins.compatibility ? api.plugins.compatibility() : null;
    if (method === 'setEnabled') return api.plugins.setEnabled(...input);
    if (method === 'commit') return withProfile(() => commitProcessProposal(input));
    if (method === 'cancelInstall') {
      if (!owner.installations.has(input)) return { status: 'not-running' };
      return api.plugins.cancelInstall(input);
    }
    if (method === 'installBundle') {
      if (typeof input?.requestId !== 'string') throw new Error('辅助进程安装缺少请求身份');
      owner.installations.add(input.requestId);
      try { return await api.plugins.installBundle(input); }
      finally { owner.installations.delete(input.requestId); }
    }
    if (method !== 'install') throw new Error('辅助进程方法未提供');
    const spec = installSpec(input, resolve(home, 'profiles', 'magi'));
    const id = randomBytes(16).toString('hex');
    owner.installations.add(id);
    try {
      const result = await withProfile(() => createPluginManager(api, registry).installBundle(spec, { enabled: true, requestId: id }), owner.controller.signal);
      return { result, snapshot: await snapshot() };
    } finally { owner.installations.delete(id); }
  }
  function listen() {
    if (server) return;
    if (process.platform === 'win32') endpoint = `\\\\.\\pipe\\magi-profile-${randomBytes(16).toString('hex')}`;
    else { directory = mkdtempSync(join(tmpdir(), 'mgi-p-')); endpoint = join(directory, 's'); }
    server = createServer(socket => {
      if (sockets.size >= 32) return socket.destroy();
      sockets.add(socket); socket.on('close', () => sockets.delete(socket));
      socket.setTimeout(900_000, () => socket.destroy());
      let data = Buffer.alloc(0), received = false;
      socket.on('error', () => {});
      socket.on('data', chunk => {
        if (received) return socket.destroy();
        data = Buffer.concat([data, chunk]);
        if (data.length > MAX_PROCESS_FRAME) return socket.destroy();
        const end = data.indexOf(10); if (end < 0) return;
        received = true;
        let message, owner;
        try {
          message = JSON.parse(data.subarray(0, end)); owner = owners.get(message.token);
          if (!owner) return socket.destroy();
          owner.sockets.add(socket); socket.on('close', () => owner.sockets.delete(socket));
        } catch { return socket.destroy(); }
        void request(owner, message.method, message.input).then(value => ({ value }), error => ({ error: { message: error.message, code: error.code } }))
          .then(reply => {
            if (socket.destroyed) return;
            let text = JSON.stringify(reply) + '\n';
            if (Buffer.byteLength(text) > MAX_PROCESS_FRAME) text = JSON.stringify({ error: { message: '辅助进程响应超过大小上限' } }) + '\n';
            socket.end(text);
          });
      });
    });
    server.on('error', () => { for (const socket of sockets) socket.destroy(); });
    server.listen(endpoint); server.unref();
  }
  return {
    spawn(file, args, options = {}, signal) {
      if (closed || file !== process.execPath || !Array.isArray(args) || !/\.(?:mjs|cjs|js)$/.test(args[0] ?? '')) return;
      if (owners.size >= 16) throw new Error('当前插件的 Node 辅助进程过多');
      signal?.throwIfAborted();
      boundary.runInAsyncScope(listen);
      const token = randomBytes(32).toString('hex'), owner = { token, installations: new Set(), sockets: new Set(), controller: new AbortController() };
      const env = { ...(options.env ?? process.env), MAGI_PROFILE_PROCESS_ENDPOINT: endpoint, MAGI_PROFILE_PROCESS_TOKEN: token,
        MAGI_PROFILE_PROCESS_HOME: home, MAGI_PROFILE_PROCESS_REGISTRY: registry };
      // 委托只持有父扩展的受限端口，不继承 daemon RPC 传输凭据。
      for (const key of ['MAGI_EXT_ENDPOINT', 'MAGI_EXT_TOKEN', 'MAGI_EXT_ENTRY']) delete env[key];
      owners.set(token, owner);
      let child;
      try { child = spawn(file, [fileURLToPath(new URL('profile-child.mjs', import.meta.url)), ...args], { ...options, env, detached: false }); }
      catch (error) { revoke(owner); throw error; }
      owner.child = child;
      const abort = () => { revoke(owner); child.kill(); };
      signal?.addEventListener('abort', abort, { once: true });
      child.once('exit', () => revoke(owner)); child.once('error', () => revoke(owner));
      child.once('close', () => signal?.removeEventListener('abort', abort));
      return child;
    },
    dispose() {
      closed = true;
      for (const owner of [...owners.values()]) { owner.child.kill(); revoke(owner); }
      for (const socket of sockets) socket.destroy();
      server?.close(() => { if (directory) rmSync(directory, { recursive: true, force: true }); });
      boundary.emitDestroy();
    },
  };
}
