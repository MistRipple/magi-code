// 槽位记录进程（运行在 Node 权限模型下，见 client-assets.mjs）：在 vm 里执行上游 ./client 的 apply，
// 记录它注册了哪些槽位。vm 只用于提供 window/document 的最小替身，真正的隔离来自进程权限。
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import vm from 'node:vm';
import { createSlotContext, slotDisposition } from '../runtime/client/slot-context.mjs';

const [clientFile, sharedRoot, resultPath] = process.argv.slice(2);
const require = createRequire(`${sharedRoot}/__shared__.cjs`);
const modules = {
  react: require('react'),
  'react-dom': require('react-dom'),
  'react/jsx-runtime': require('react/jsx-runtime'),
};
const moduleStubs = new Map();
const stubModule = (name) => {
  if (!moduleStubs.has(name)) moduleStubs.set(name, new Proxy({}, { get: (_target, key) => {
    if (key === '__esModule') return true;
    return (..._args) => null;
  } }));
  return moduleStubs.get(name);
};
const scope = { getSnapshot: () => ({ value: {} }), subscribe: () => () => {}, set: async () => true };
const slotContext = createSlotContext({ scope });
let applied = false;
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
  window: undefined, document: { querySelector: () => null, querySelectorAll: () => [], getElementById: (id) => elements.get(id) ?? null, createElement: element, createTextNode: (text) => ({ textContent: String(text) }), head: element('head'), body: element('body'), documentElement: element('html'), addEventListener() {}, removeEventListener() {} },
  navigator: { language: 'zh-CN' }, console, setTimeout, clearTimeout, setInterval, clearInterval, URL, URLSearchParams, Blob: globalThis.Blob,
  AbortController, TextEncoder, TextDecoder, queueMicrotask, fetch: () => Promise.reject(new Error('记录阶段不发送请求')),
};
sandbox.window = sandbox;
sandbox.window.addEventListener = () => {};
sandbox.window.removeEventListener = () => {};
sandbox.window.__ModuleLoader__ = {
  load({ factory }) {
    const exports = factory((name) => {
      if (name in modules) return modules[name];
      if (name.startsWith('@deepseek-ai/dsh-client-ui-')) return stubModule(name);
      throw new Error(`上游界面需要未提供的模块：${name}`);
    });
    if (typeof exports.apply !== 'function') throw new Error('上游界面没有导出 apply');
    exports.apply(slotContext.ctx);
    applied = true;
  },
};
vm.runInNewContext(readFileSync(clientFile, 'utf8'), sandbox, { timeout: 20_000 });
if (!applied) throw new Error('上游界面没有调用 __ModuleLoader__.load');
const slots = new Map();
for (const entry of slotContext.live()) slots.set(entry.descriptor.name, (slots.get(entry.descriptor.name) ?? 0) + 1);
writeFileSync(resultPath, JSON.stringify([...slots].map(([name, count]) => ({ name, count, disposition: slotDisposition(name) }))));
process.exit(0);
