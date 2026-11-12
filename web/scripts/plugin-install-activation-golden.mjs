import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => value;

await withGoldenViteServer(async (server) => {
  const events = [];
  globalThis.window = {
    location: new URL('http://127.0.0.1:38123/web.html'),
    dispatchEvent: (event) => { events.push(event.type); return true; },
    localStorage: { getItem: () => null, setItem: () => {} },
  };
  globalThis.CustomEvent = class { constructor(type) { this.type = type; } };
  const storage = new Map();
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => { storage.set(key, String(value)); }, removeItem: (key) => { storage.delete(key); } } });

  const permissions = [{ kind: 'storage' }];
  const plugin = (over = {}) => ({
    manifest: { id: 'p.demo', version: '1.0.0', name: 'demo', description: '', permissions, contributions: {} },
    digest: 'digest-new', grants: [], source: { kind: 'local', location: '/x.zip' }, enabled: false, active: false, ...over,
  });

  let installed = [];
  let failActivate = false;
  const calls = [];
  globalThis.fetch = async (url, init = {}) => {
    const path = new URL(url, 'http://127.0.0.1:38123').pathname;
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ path, body });
    const json = (value, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
    if (path === '/api/plugins' && (init.method ?? 'GET') === 'GET') return json({ plugins: installed });
    if (/\/(install|upgrade|center\/install|upstream\/install)$/.test(path)) { installed = [plugin()]; return json({}); }
    if (path.endsWith('/authorize')) { installed = installed.map((item) => ({ ...item, grants: body.grants })); return json({}); }
    if (path.endsWith('/enable')) { installed = installed.map((item) => ({ ...item, enabled: true })); return json({}); }
    if (path.endsWith('/activate')) {
      if (failActivate) return json({ message: 'runtime unavailable' }, 409);
      installed = installed.map((item) => ({ ...item, active: true })); return json({});
    }
    return json({}, 404);
  };

  const api = await server.ssrLoadModule('/src/web/plugin-api.ts');
  const lifecycle = () => calls.filter(({ path }) => /\/(install|upgrade|authorize|enable|activate)$/.test(path)).map(({ path }) => path.split('/').slice(-1)[0]);
  const preview = { manifest: plugin().manifest, digest: 'digest-new', operation: 'install', addedPermissions: permissions, removedPermissions: [] };

  // 本地/地址安装：确认后一次完成 安装 → 授权 → 启用 → 激活，授权内容就是预览里展示的权限。
  installed = []; calls.length = 0;
  await api.applyPluginCandidate({ source: 'local:x' }, preview, true);
  assert.deepEqual(lifecycle(), ['install', 'authorize', 'enable', 'activate']);
  assert.deepEqual(calls.find(({ path }) => path.endsWith('/authorize')).body, { expectedDigest: 'digest-new', grants: permissions });
  assert.equal(installed[0].active, true);

  // 调用方不要求激活（如原本被停用的插件升级）时只安装。
  installed = []; calls.length = 0;
  await api.applyPluginCandidate({ source: 'local:x' }, { ...preview, operation: 'upgrade' }, false);
  assert.deepEqual(lifecycle(), ['upgrade']);
  assert.equal(installed[0].active, false);

  // 上游与插件中心安装走同一默认激活。
  installed = []; calls.length = 0;
  await api.installUpstreamPlugin({ package: 'dsh-x', version: '1.0.0', candidate: preview }, 'dsh', true);
  assert.deepEqual(lifecycle(), ['install', 'authorize', 'enable', 'activate']);
  installed = []; calls.length = 0;
  await api.installFromPluginCenter({ id: 'p.demo', version: '1.0.0', sha256: 'x' }, true);
  assert.deepEqual(lifecycle(), ['install', 'authorize', 'enable', 'activate']);

  // 安装后的包摘要与预览不一致（被替换）时不授权。
  installed = []; calls.length = 0;
  await assert.rejects(api.applyPluginCandidate({ source: 'local:x' }, { ...preview, digest: 'digest-other' }, true));
  assert.deepEqual(lifecycle(), ['install'], '摘要不一致时不能对被替换的版本授权');

  // 激活失败：安装已提交，错误如实抛出（调用方刷新并提示），不假装成功。
  installed = []; calls.length = 0; failActivate = true;
  await assert.rejects(api.applyPluginCandidate({ source: 'local:x' }, preview, true), /runtime unavailable/);
  assert.equal(installed[0].active, false);
  failActivate = false;

  delete globalThis.window; delete globalThis.CustomEvent; delete globalThis.localStorage;
  console.log('plugin install activation golden passed');
});
