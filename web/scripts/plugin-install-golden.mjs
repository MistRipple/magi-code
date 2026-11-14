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
    digest: 'digest-new', source: { kind: 'local', location: '/x.zip' }, enabled: true, ...over,
  });

  let installed = [];
  let failLifecycle = false;
  const calls = [];
  globalThis.fetch = async (url, init = {}) => {
    const path = new URL(url, 'http://127.0.0.1:38123').pathname;
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ path, body });
    const json = (value, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
    if (path === '/api/plugins' && (init.method ?? 'GET') === 'GET') return json({ plugins: installed });
    // 安装完成即启用；升级保持原启用状态。
    if (/\/(install|center\/install|upstream\/install)$/.test(path)) { installed = [plugin()]; return json({}); }
    if (path.endsWith('/upgrade')) { installed = installed.map((item) => ({ ...item, digest: 'digest-new' })); return json({}); }
    if (/\/(enable|disable)$/.test(path)) {
      if (failLifecycle) return json({ message: 'runtime unavailable' }, 409);
      installed = installed.map((item) => ({ ...item, enabled: path.endsWith('/enable') }));
      return json({});
    }
    return json({}, 404);
  };

  const api = await server.ssrLoadModule('/src/web/plugin-api.ts');
  const lifecycle = () => calls.filter(({ path }) => /\/(install|upgrade|enable|disable)$/.test(path)).map(({ path }) => path.split('/').slice(-1)[0]);
  const preview = { manifest: plugin().manifest, digest: 'digest-new', operation: 'install', addedPermissions: permissions, removedPermissions: [] };

  // 本地/地址安装：确认后只有一次安装请求，daemon 提交即启用，前端不再追加授权或激活。
  installed = []; calls.length = 0;
  await api.applyPluginCandidate({ source: 'local:x' }, preview);
  assert.deepEqual(lifecycle(), ['install']);
  assert.equal(installed[0].enabled, true);

  // 升级保持原启用状态：被用户停用的插件升级后仍是停用。
  installed = [plugin({ digest: 'digest-old', enabled: false })]; calls.length = 0;
  await api.applyPluginCandidate({ source: 'local:x' }, { ...preview, operation: 'upgrade' });
  assert.deepEqual(lifecycle(), ['upgrade']);
  assert.equal(installed[0].enabled, false);

  // 上游与插件中心安装同样只有一次安装请求。
  installed = []; calls.length = 0;
  await api.installUpstreamPlugin({ package: 'dsh-x', version: '1.0.0', candidate: preview }, 'dsh');
  assert.deepEqual(lifecycle(), ['install']);
  installed = []; calls.length = 0;
  await api.installFromPluginCenter({ id: 'p.demo', version: '1.0.0', sha256: 'x' });
  assert.deepEqual(lifecycle(), ['install']);

  // 启停是单次请求并绑定用户看到的摘要。
  calls.length = 0;
  await api.disablePlugin(installed[0]);
  assert.deepEqual(lifecycle(), ['disable']);
  assert.deepEqual(calls[0].body, { expectedDigest: 'digest-new' });
  assert.equal(installed[0].enabled, false);
  await api.enablePlugin(installed[0]);
  assert.equal(installed[0].enabled, true);

  // 启停失败：错误如实抛出并仍刷新注册表，不假装成功。
  failLifecycle = true; events.length = 0;
  await assert.rejects(api.disablePlugin(installed[0]), /runtime unavailable/);
  assert.equal(installed[0].enabled, true);
  assert.ok(events.length > 0, '失败也要刷新已提交事实');

  delete globalThis.window; delete globalThis.CustomEvent; delete globalThis.localStorage;
  console.log('plugin install golden passed');
});
