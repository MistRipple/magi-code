// DSH 浏览器端宿主垫片：让上游自带的 `./client` 界面（React 槽位体系）在 Magi 的沙箱视图页里原样运行。
//
// 做什么：
//   - 提供上游界面需要的 `__ModuleLoader__`（react / react-dom / jsx-runtime 由本页提供，与上游同一份）；
//   - 提供 `ctx.slots` / `ctx.locale` / `ctx.effect` / `ctx.get`，把上游注册的槽位收集起来；
//   - 在 Magi 的 settings 视图里渲染上游注册在“插件配置”位置的卡片（`settings.section` 或较早的 `plugins.bundle.config`），整页形态；
//   - `fetch('/plugins/<id>/…')` 改走视图桥的 route.call，调用的仍是宿主里的同一处理器；
//   - `settingsScope` 由 Magi 设置（settings.read / settings.write）支撑；
//   - 主题令牌映射到上游使用的 `--dsw-*` 变量，语言随 Magi 切换。
//
// 不做什么（如实披露，见兼容报告）：DSH 外壳里的其他槽位（输入框右侧、会话底部栏、侧栏面板、全局配额提示等）
// 属于 DSH 的会话界面，Magi 没有对应位置；这些注册被收集后不渲染，并记录在 `unmountedSlots` 里供报告读取。
import * as React from 'react';
import * as ReactDOM from 'react-dom';
import * as ReactDOMClient from 'react-dom/client';
import * as JsxRuntime from 'react/jsx-runtime';
import { createSlotContext, slotDisposition, MOUNT_PRIORITY, SLOT_SURFACE } from './slot-context.mjs';
import { createSettingsScope } from './settings-scope.mjs';
import { bindSlotProps } from './slot-bindings.mjs';

const CHANNEL = 'magi-plugin-v1';

/**
 * @param {object} options
 * @param {{ kind: 'exact' | 'prefix', path: string }[]} options.routes 构建期记录的后端路由；界面只能访问这些路径
 * @param {string} [options.clientScript] 上游界面脚本相对路径，默认 client.js
 * @param {string} [options.slotName] 只渲染这个上游槽位（右栏/主页面入口）
 */
export function startDshClient(options) {
  const modules = { react: React, 'react-dom': ReactDOM, 'react/jsx-runtime': JsxRuntime };
  globalThis.__MAGI_DSH_MODULES__ = modules;
  let locale = 'zh-CN';
  let nextRequest = 0;
  let instanceId = null;
  let placement = 'page';
  const pending = new Map();

  // —— 视图桥 ——
  const post = (message) => parent.postMessage({ channel: CHANNEL, instanceId, type: 'request', ...message }, '*');
  const request = (message) => new Promise((resolve, reject) => {
    const requestId = `r${++nextRequest}`;
    pending.set(requestId, { resolve, reject });
    post({ requestId, ...message });
  });

  // —— 路由：上游界面用 fetch('/plugins/<name>/…') 访问自己的后端，这里转交给宿主里的同一处理器 ——
  globalThis.fetch = async (input, init = {}) => {
    const raw = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    const url = new URL(raw, 'http://magi.invalid');
    if (url.origin !== 'http://magi.invalid') {
      throw new TypeError(`插件界面只能访问自己宿主的后端路由：${raw}`);
    }
    const method = String(init.method ?? 'GET').toUpperCase();
    let body;
    if (init.body !== undefined && init.body !== null) {
      const text = typeof init.body === 'string' ? init.body : await new Response(init.body).text();
      try { body = JSON.parse(text); } catch { body = text; }
    }
    const result = await request({ operation: 'route.call', method, path: url.pathname, query: url.search.replace(/^\?/, ''), body });
    const contentType = result.contentType ?? (typeof result.body === 'string' ? 'text/plain' : 'application/json');
    const payload = result.body === null || result.body === undefined ? '' : typeof result.body === 'string' ? result.body : JSON.stringify(result.body);
    return new Response([204, 205, 304].includes(result.status) ? null : payload, { status: result.status, headers: { 'content-type': contentType } });
  };

  // —— 沙箱里被禁的窗口、剪贴板、下载：经桥交给宿主 ——
  window.open = (url) => { if (url) void request({ operation: 'host.openExternal', url: String(url) }).catch(() => {}); return null; };
  // 沙箱 iframe 禁止弹出窗口；把上游原生外链交给宿主，保留 href、target、rel
  // 和页面代码本身。文档内锚点仍由浏览器原样处理。
  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null;
    if (!anchor) return;
    const url = new URL(anchor.getAttribute('href') ?? '', document.baseURI);
    if (!['http:', 'https:', 'mailto:'].includes(url.protocol)) return;
    event.preventDefault();
    void request({ operation: 'host.openExternal', url: url.toString() }).catch(() => {});
  }, true);
  try {
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: (text) => request({ operation: 'clipboard.write', text: String(text) }) }, configurable: true });
  } catch { /* 浏览器不允许覆盖时保持原样，复制按钮会得到明确的权限错误 */ }

  // —— settingsScope：上游界面读写配置的接口，背后是 Magi 的 settings.read / settings.write ——
  const { scope, refresh: refreshSettings } = createSettingsScope(request);
  const connection = { rpc: { async call(path, endpoint, input) {
    if (path !== '/api' || !input || Object.keys(input).some((key) => key !== 'args')) throw new Error('插件 Remote 仅支持 /api 的 JSON unary 调用');
    const result = await request({ operation: 'route.call', method: 'POST', path: '/__magi_compat/remote', body: { endpoint, args: input.args } });
    return result.body;
  } } };

  // —— ctx：与构建期记录共用同一份实现 ——
  const slotContext = createSlotContext({ scope, connection, onChange: () => render() });
  const { ctx } = slotContext;

  // —— 主题：Magi 令牌映射到上游的 --dsw-* 变量 ——
  const applyTheme = (theme) => {
    const root = document.documentElement;
    for (const [name, value] of Object.entries(theme.tokens ?? {})) root.style.setProperty(name, value);
    const t = theme.tokens ?? {};
    const alias = {
      // 上游三级文字色：primary 标题/正文，secondary 控件与次要正文（按钮、下拉都用它，必须清晰），tertiary/dimmed 说明与弱化。
      // 宿主的“设置页次级文字色”正是这一级；不可用时退回正文色，而不是更淡的弱化色。
      '--dsw-alias-label-primary': t['--foreground'], '--dsw-alias-label-secondary': t['--ind-foreground-secondary'] ?? t['--foreground'],
      '--dsw-alias-label-tertiary': t['--foreground-muted'], '--dsw-alias-label-dimmed': t['--foreground-muted'],
      '--dsw-alias-border-l1': t['--border-subtle'], '--dsw-alias-border-l2': t['--border'],
      '--dsw-alias-bg-base': t['--background'], '--dsw-alias-bg-layer-1': t['--background'],
      '--dsw-alias-bg-layer-2': t['--surface-2'], '--dsw-alias-bg-layer-3': t['--surface-3'], '--dsw-alias-bg-l1': t['--surface-1'],
      '--dsw-alias-brand-primary': t['--primary'], '--dsw-alias-interactive-bg-hover': t['--surface-hover'],
      '--dsw-alias-link': t['--primary'],
      '--dsw-alias-state-success-primary': t['--success'], '--dsw-alias-state-warn-primary': t['--warning'],
      '--dsw-alias-state-error-primary': t['--error'], '--dsw-alias-label-error': t['--error'],
      '--dsw-alias-label-success': t['--success'], '--dsw-alias-label-warning': t['--warning'], '--dsw-alias-label-caution': t['--warning'],
    };
    for (const [name, value] of Object.entries(alias)) if (value) root.style.setProperty(name, value);
    root.style.colorScheme = theme.mode === 'light' ? 'light' : 'dark';
    root.dataset.theme = theme.mode;
    const embedded = placement === 'settings';
    // 字号与字体取宿主下发的令牌；缺失时才回退到继承，不写死数值。
    const font = t['--vscode-font-family'] ? `${t['--text-base'] ?? 'inherit'}/1.5 ${t['--vscode-font-family']}` : 'inherit';
    document.body.style.cssText = `margin:0;background:${embedded ? 'transparent' : (t['--background'] ?? 'transparent')};color:${t['--foreground'] ?? 'inherit'};font:${font}`;
    applyPlacement();
  };

  // —— 位置：嵌入设置页时由宿主整页滚动，页面按内容高度上报，自己不再出现滚动条 ——
  let lastReported = -1;
  let reportScheduled = false;
  const reportHeight = () => {
    reportScheduled = false;
    if (placement !== 'settings' || instanceId === null) return;
    const height = Math.ceil(document.getElementById('root')?.getBoundingClientRect().height ?? 0);
    if (height === lastReported) return;
    lastReported = height;
    void request({ operation: 'view.resize', height }).catch(() => {});
  };
  const scheduleReport = () => {
    if (reportScheduled) return;
    reportScheduled = true;
    requestAnimationFrame(reportHeight);
  };
  // 嵌入设置页时的版面规则。外层宽度、左右留白、与页头的间距、圆角、滚动都由宿主的设置框架决定，
  // 页面只负责内容：顶层容器去掉自带的视口约束与留白，铺满宿主给的内容列。
  // 选择器带 html[data-placement] 与 #root，优先级高于上游的类选择器，因此不依赖 !important；
  // 取值只用宿主下发的令牌，其他位置（placement=page）不受影响。
  // 实心按钮的文字色：上游把叠层背景别名（半透明，如 bg-layer-3）当文字色用在“实心主色底”的控件上，
  // 在宿主主题下几乎不可见。这些位置改用宿主主按钮的前景色（与 Magi 原生主按钮同一对反色）。
  // 只覆盖已知的、底色为 label-primary 的控件；其他叠层背景的用法保持不变。
  const SOLID_CONTROLS = ['.dsm-btn-primary', '.dsm-workbuddy-device-seg-on'];
  // 页签是可点击控件，上游却用最淡的“说明”级文字色（label-tertiary），浅色主题下对比度不足 4.5:1。
  // 交互控件用次级文字色（与按钮同一级）；选中态与悬停态由上游自己的更具体规则覆盖，不受影响。
  const TAB_CONTROLS = ['.dsm-workbuddy-tab', '.dsm-workbuddy-panel-tab'];
  const embedStyle = document.createElement('style');
  embedStyle.textContent = [
    'html[data-placement="settings"] #root > * {',
    '  height: auto; min-height: 0; max-height: none; overflow: visible;',
    '  width: 100%; max-width: none; margin: 0; padding: 0; align-items: stretch;',
    '}',
    'html[data-placement="settings"] #root > * > * { max-width: none; }',
    `html[data-theme] :is(${SOLID_CONTROLS.join(', ')}) { color: var(--btn-primary-fg, var(--magi-canvas, #fff)); }`,
    `html[data-theme] :is(${TAB_CONTROLS.join(', ')}):not(.dsm-workbuddy-tab-active):not(.dsm-workbuddy-panel-tab-active):not(:hover) { color: var(--dsw-alias-label-secondary); }`,
  ].join('\n');
  document.head.appendChild(embedStyle);
  const applyPlacement = () => {
    const embedded = placement === 'settings';
    // 滚动归宿主的设置框架：页面本身不产生滚动条，高度随内容上报。
    document.documentElement.style.overflow = embedded ? 'hidden' : '';
    document.body.style.overflow = embedded ? 'hidden' : '';
    document.documentElement.dataset.placement = placement;
    scheduleReport();
  };
  new ResizeObserver(scheduleReport).observe(document.getElementById('root'));

  // —— 渲染：把上游注册在“插件配置”位置的卡片整页渲染出来 ——
  const container = document.getElementById('root');
  const root = ReactDOMClient.createRoot(container);
  const render = () => {
    const cards = slotContext.live().filter((entry) => options.slotName
      ? entry.descriptor.name === options.slotName
      : slotDisposition(entry.descriptor.name) === 'mounted');
    // 同一张卡片可能以多个槽位注册（DSH 的不同入口与行布局）；Magi 只有一种页面，按优先级取一份。
    const rank = (entry) => { const index = MOUNT_PRIORITY.indexOf(entry.descriptor.name); return index === -1 ? MOUNT_PRIORITY.length : index; };
    const card = [...cards].sort((a, b) => rank(a) - rank(b))[0];
    if (!card) {
      root.render(React.createElement('div', { style: { padding: 16 } }, '该插件没有提供设置界面。'));
      return;
    }
    const injected = bindSlotProps(card.descriptor, card.descriptor.inject?.() ?? {}, ctx.locale);
    root.render(React.createElement(card.component, { ...injected, settingsScope: scope, surface: SLOT_SURFACE[card.descriptor.name] ?? 'page', view: 'page' }));
  };

  /** 上游界面是一个调用 `__ModuleLoader__.load` 的脚本文件；以 <script src> 加载，页面 CSP 不允许 eval。 */
  const loadScript = (src) => new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = src;
    script.onload = () => resolve();
    script.onerror = () => reject(new Error(`加载 ${src} 失败`));
    document.head.appendChild(script);
  });
  const loadUpstream = async (src) => {
    for (const file of options.libraryScripts ?? []) await loadScript(file);
    let initialized;
    window.__ModuleLoader__ = {
      load({ factory }) {
        try {
          const exports = factory((name) => {
            if (!Object.hasOwn(modules, name)) throw new Error(`上游界面需要未提供的模块：${name}`);
            return modules[name];
          });
          // script.onload 只说明文件加载成功；上游 apply 的异步失败也必须进入错误界面。
          initialized = Promise.resolve(exports.apply(ctx)).then(() => ({}), (error) => ({ error }));
        } catch (error) {
          initialized = Promise.resolve({ error });
        }
      },
    };
    await loadScript(src);
    if (!initialized) throw new Error('上游界面未通过 ModuleLoader 初始化');
    const result = await initialized;
    if (result.error) throw result.error;
  };

  window.addEventListener('message', async (event) => {
    const m = event.data;
    if (!m || m.channel !== CHANNEL) return;
    if (m.type === 'init') {
      instanceId = m.instanceId;
      placement = m.placement === 'settings' ? 'settings' : 'page';
      lastReported = -1;
      locale = m.locale ?? locale;
      slotContext.setLocale(locale);
      if (m.theme) applyTheme(m.theme);
      try {
        await refreshSettings();
        await loadUpstream(options.clientScript ?? 'client.js');
        render();
      } catch (error) {
        root.render(React.createElement('div', { style: { padding: 16, color: 'crimson' } }, `界面加载失败：${error?.message ?? error}`));
      }
    } else if (m.type === 'context') {
      if (m.placement) { placement = m.placement === 'settings' ? 'settings' : 'page'; }
      if (m.locale && m.locale !== locale) { locale = m.locale; slotContext.setLocale(locale); render(); }
      if (m.theme) applyTheme(m.theme);
    } else if (m.type === 'response') {
      const waiter = pending.get(m.requestId);
      pending.delete(m.requestId);
      if (waiter) m.ok ? waiter.resolve(m.result) : waiter.reject(new Error(m.error));
    }
  });
}
