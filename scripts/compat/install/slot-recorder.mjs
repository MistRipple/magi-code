// 槽位记录进程（运行在 Node 权限模型下，见 client-assets.mjs）：在 vm 里执行上游 ./client 的 apply，
// 记录它注册了哪些槽位。vm 只用于提供 window/document 的最小替身，真正的隔离来自进程权限。
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import vm from 'node:vm';
import { createSlotContext, slotDisposition, SLOT_SURFACE } from '../runtime/client/slot-context.mjs';
import { browserLibraries } from '../runtime/client/modules.mjs';

const [clientFile, sharedRoot, resultPath] = process.argv.slice(2);
const require = createRequire(`${sharedRoot}/__shared__.cjs`);
const modules = {
  react: require('react'),
  'react-dom': require('react-dom'),
  'react/jsx-runtime': require('react/jsx-runtime'),
};
const snapshot = { status: 'ready', value: {}, base: {}, user: {}, writable: false, revision: 0 };
const scope = { getSnapshot: () => snapshot, subscribe: () => () => {}, set: async () => false };
const slotContext = createSlotContext({ scope });
let applied = false;
let initialized;
// 最小的浏览器环境：模块加载时注入 <style> 的写法很常见，给一个无副作用的 document。
const elements = new Map();
const element = (tag = 'div') => {
  const node = { tagName: tag.toUpperCase(), dataset: {}, style: {}, children: [], textContent: '',
    appendChild(child) { this.children.push(child); if (child?.id) elements.set(child.id, child); return child; },
    remove() { if (this.id) elements.delete(this.id); }, setAttribute(name, value) { this[name] = String(value); if (name === 'id') elements.set(this.id, this); },
    addEventListener() {}, removeEventListener() {}, querySelector() { return null; }, querySelectorAll() { return []; } };
  return node;
};
const sandbox = {
  window: undefined, document: { compatMode: 'CSS1Compat', querySelector: () => null, querySelectorAll: () => [], getElementById: (id) => elements.get(id) ?? null, createElement: element, createTextNode: (text) => ({ textContent: String(text) }), head: element('head'), body: element('body'), documentElement: element('html'), addEventListener() {}, removeEventListener() {} },
  navigator: { language: 'zh-CN', userAgent: '', platform: process.platform }, console, setTimeout, clearTimeout, setInterval, clearInterval, URL, URLSearchParams, Blob: globalThis.Blob,
  AbortController, TextEncoder, TextDecoder, queueMicrotask, fetch: () => Promise.reject(new Error('记录阶段不发送请求')),
  __MAGI_DSH_MODULES__: modules,
};
sandbox.window = sandbox;
sandbox.window.addEventListener = () => {};
sandbox.window.removeEventListener = () => {};
const context = vm.createContext(sandbox);
const browserManifest = JSON.parse(readFileSync(`${sharedRoot}/dist/browser-modules.json`, 'utf8'));
const loadLibrary = (name) => {
  // 与真实页面共用随包预构建模块。不能用“任何导出都返回 null”的替身：
  // 上游 apply 中构造 SettingsFormModel 等真实服务时会失败或漏掉注册。
  for (const library of browserLibraries([name])) {
    if (Object.hasOwn(modules, library.name)) continue;
    for (const file of browserManifest[library.name] ?? []) {
      if (!file.endsWith('.js')) continue;
      if (!/^[a-z0-9-]+\.js$/.test(file)) throw new Error('浏览器模块路径无效');
      vm.runInContext(readFileSync(`${sharedRoot}/dist/${file}`, 'utf8'), context, { timeout: 20_000, filename: file });
    }
    if (!Object.hasOwn(modules, library.name)) throw new Error(`浏览器模块没有初始化：${library.name}`);
  }
  return modules[name];
};
sandbox.window.__ModuleLoader__ = {
  load({ factory }) {
    const exports = factory((name) => {
      if (name in modules) return modules[name];
      return loadLibrary(name);
    });
    if (typeof exports.apply !== 'function') throw new Error('上游界面没有导出 apply');
    initialized = Promise.resolve(exports.apply(slotContext.ctx));
    initialized.catch(() => {});
    applied = true;
  },
};
try {
  vm.runInContext(readFileSync(clientFile, 'utf8'), context, { timeout: 20_000 });
  if (!applied) throw new Error('上游界面没有调用 __ModuleLoader__.load');
  await initialized;
  const labelOf = (label) => { const value = typeof label === 'function' ? label() : label; return typeof value === 'string' && value ? value : undefined; };
  const live = slotContext.live();
  // 同一配置组件在上游外壳的 main 别名不成为第二个编辑入口；独立业务组件保留。
  const settingsComponents = new Set(live.filter(entry => SLOT_SURFACE[entry.descriptor.name] === 'settings').map(entry => entry.component));
  const slots = live.map(({ descriptor, component }) => {
    const { name } = descriptor;
    const key = String(descriptor.key ?? descriptor.id ?? '');
    const tab = slotContext.tabTypes().find((item) => item.id === key);
    const navigation = slotContext.live().find((item) => item.descriptor.name === 'sidebar.panellist' && item.descriptor.id === key);
    const settingsDuplicate = name === 'main' && settingsComponents.has(component);
    return { name, key, label: labelOf(descriptor.label ?? tab?.title ?? navigation?.descriptor.label),
      ...(descriptor.priority === undefined ? {} : { priority: descriptor.priority }), count: 1,
      ...(settingsDuplicate ? { settingsDuplicate: true } : {}), disposition: settingsDuplicate ? 'alternate' : slotDisposition(name, key) };
  });
  // 与真实文档关闭采用同一清理合同；清理失败也属于原入口未完成，不能交付
  // 一份只记录了注册、却从未验证释放函数可调用的成功报告。
  slotContext.dispose();
  writeFileSync(resultPath, JSON.stringify(slots));
} catch (error) {
  // 安装准入失败作为用户可读原因传回 daemon，不把 Node 堆栈当作产品提示。
  console.error(error.message);
  process.exitCode = 1;
} finally {
  process.exit(process.exitCode ?? 0);
}
