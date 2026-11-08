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
// 不做什么（如实披露，见兼容报告）：DSH 外壳里的其他槽位（输入框右侧、会话底部栏、全局配额提示等）
// 属于 DSH 的会话界面，Magi 没有对应位置；这些注册被收集后不渲染，并记录在 `unmountedSlots` 里供报告读取。
import * as React from 'react';
import * as ReactDOM from 'react-dom';
import * as ReactDOMClient from 'react-dom/client';
import * as JsxRuntime from 'react/jsx-runtime';
import { createSlotContext, slotDisposition, MOUNT_PRIORITY, SLOT_SURFACE } from './slot-context.mjs';
import { createSettingsScope } from './settings-scope.mjs';
import { bindSlotProps } from './slot-bindings.mjs';
import { createWebStorage } from './web-storage.mjs';
import { createEventSource } from './event-source.mjs';
import { createComposerClient } from './composer.mjs';
import { createSessionComposerServices } from './session-composer.mjs';
import { SettingsForm } from './settings-form.mjs';
import { createViewTransport } from './view-transport.mjs';
import { createRouteFetch } from './route-fetch.mjs';
import { createRemoteConnection } from './connection.mjs';

const CHANNEL = 'magi-plugin-v1';

/**
 * @param {object} options
 * @param {string} [options.clientScript] 上游界面脚本相对路径，默认 client.js
 * @param {string} [options.slotName] 只渲染这个上游槽位（右栏/主页面入口）
 */
export function startDshClient(options) {
  const modules = { react: React, 'react-dom': ReactDOM, 'react/jsx-runtime': JsxRuntime };
  globalThis.__MAGI_DSH_MODULES__ = modules;
  let locale = 'zh-CN';
  let instanceId = null;
  let placement = 'page';
  let selectedSlot = options;
  let initialized = false;
  let settingsSchema, settingsError;
  const selectView = (m) => {
    placement = ['settings', 'background'].includes(m.placement) ? m.placement : 'page';
    if (options.clientViews?.length && m.viewId) {
      const view = options.clientViews.find(view => view.id === m.viewId);
      if (!view) throw new Error('插件视图未声明');
      selectedSlot = { slotName: view.name, slotKey: view.key };
    }
  };
  // —— 视图桥 ——
  const transport = createViewTransport(message => parent.postMessage({ channel: CHANNEL, instanceId, ...message }, '*'));
  const request = transport.request;
  const eventSources = createEventSource(request, document.baseURI);
  const composer = createComposerClient(request);
  const sessionComposer = options.business?.operations?.length ? createSessionComposerServices(composer.api) : undefined;
  globalThis.EventSource = eventSources.EventSource;
  window.addEventListener('pagehide', () => { eventSources.dispose(); composer.dispose(); sessionComposer?.dispose(); slotContext.dispose(); transport.dispose(); }, { once: true });

  // —— 路由：上游界面用 fetch('/plugins/<name>/…') 访问自己的后端，这里转交给宿主里的同一处理器 ——
  globalThis.fetch = createRouteFetch(request, document.baseURI);

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
  const connection = createRemoteConnection(request);

  // —— ctx：与构建期记录共用同一份实现 ——
  const slotContext = createSlotContext({ scope, connection, composer: sessionComposer, onChange: () => render(), onOpenTab: (key) => {
    const view = options.panelViews?.find((item) => item.key === key);
    if (!view) throw new Error(`右栏视图未声明：${key}`);
    return request({ operation: 'view.open', viewId: view.viewId });
  } });
  const { ctx } = slotContext;
  ctx.magiComposer = composer.api;

  // —— 主题：Magi 令牌映射到上游的 --dsw-* 变量 ——
  const applyTheme = (theme) => {
    const root = document.documentElement;
    for (const [name, value] of Object.entries(theme.tokens ?? {})) root.style.setProperty(name, value);
    const t = theme.tokens ?? {};
    // DSH 的层级色是完整颜色，也会被原包用作反色文字；Magi 的
    // surface-* 是用于叠加的透明材质，不能直接充当这些公共语义色。
    const canvas = t['--magi-canvas'] ?? t['--background'];
    const secondary = t['--foreground-secondary'];
    const alias = {
      // secondary/tertiary 都可能承载可读正文与操作；沿共享设置页的文字层级，
      // 不把 Magi 的弱化色直接用于原包可点击页签。dimmed 保留弱化语义。
      '--dsw-alias-label-primary': t['--foreground'], '--dsw-alias-label-secondary': secondary,
      '--dsw-alias-label-tertiary': secondary, '--dsw-alias-label-dimmed': t['--foreground-muted'],
      '--dsw-alias-border-l1': t['--border-subtle'], '--dsw-alias-border-l2': t['--border'],
      '--dsw-alias-bg-base': t['--background'], '--dsw-alias-bg-layer-1': t['--background'],
      '--dsw-alias-bg-layer-2': t['--dropdown-bg'],
      '--dsw-alias-bg-layer-3': canvas,
      '--dsw-alias-bg-l1': t['--surface-1'],
      '--dsw-alias-brand-primary': t['--primary'], '--dsw-alias-interactive-bg-hover': t['--surface-hover'],
      '--dsw-alias-link': t['--primary'],
      '--dsw-alias-state-success-primary': t['--success'], '--dsw-alias-state-warn-primary': t['--warning'],
      '--dsw-alias-state-error-primary': t['--error'], '--dsw-alias-label-error': t['--error'],
      '--dsw-alias-label-success': t['--success'], '--dsw-alias-label-warning': t['--warning'], '--dsw-alias-label-caution': t['--warning'],
      '--dsw-alias-label-primary-foreground': t['--btn-primary-fg'] ?? t['--primary-foreground'],
      '--dsw-alias-button-primary-fill': t['--btn-primary-bg'] ?? t['--primary'],
      '--dsw-alias-button-primary-hover': t['--btn-primary-hover-bg'] ?? t['--primary-hover'],
      '--dsw-alias-button-tool-bar-fill': t['--surface-2'], '--dsw-alias-button-tool-bar-hover': t['--surface-hover'],
      '--dsw-alias-interactive-bg-active': t['--surface-active'], '--dsw-alias-interactive-bg-hover-danger': t['--error-muted'],
      '--dsw-alias-bg-mask-1': t['--overlay'], '--dsw-alias-bg-skeleton': t['--surface-2'],
      '--dsw-alias-bg-module-platform': t['--dropdown-bg'], '--dsw-alias-border-l3': t['--border'], '--dsw-alias-border-l4': t['--border'],
      '--dsw-alias-switch-thumb': t['--primary-foreground'], '--dsw-alias-label-caption': t['--foreground-muted'],
      '--dsw-alias-tooltip-bg': t['--dropdown-bg'], '--dsw-alias-tooltip-key-bg': t['--surface-2'],
      '--dsw-alias-toast-bg': t['--dropdown-bg'], '--dsw-alias-toast-label': t['--foreground'],
      '--dsw-alias-menu-icon': t['--foreground'], '--dsw-alias-menu-group-header-fill': t['--surface-2'],
      '--dsw-alias-scrollbar-bg-l2': t['--scrollbar-thumb'], '--dsw-alias-scrollbar-hover-l2': t['--scrollbar-thumb-hover'],
      '--dsw-alias-markdown-code-block': t['--code-bg'], '--dsw-alias-markdown-code-block-banner': t['--surface-2'],
      '--dsw-radius-xs': t['--radius-xs'], '--dsw-radius-sm': t['--radius-sm'], '--dsw-radius-md': t['--radius-md'],
      '--dsw-radius-lg': t['--radius-lg'], '--dsw-radius-xl': t['--radius-xl'], '--dsw-radius-panel': t['--radius-2xl'],
      '--dsw-elevation-soft': t['--shadow-sm'], '--dsw-elevation-panel': t['--shadow-sm'], '--dsw-elevation-prominent': t['--shadow-md'],
      '--dsw-shadow-lv3': t['--shadow-lg'], '--dsw-menu-surface-fill': t['--dropdown-bg'], '--dsw-hovercard-bg': t['--dropdown-bg'],
      '--dsw-focus-ring-color': t['--primary'], '--dsw-focus-ring-width': '2px', '--dsw-mask-blur': 'none',
      '--ds-transition-duration': t['--duration-normal'], '--ds-ease-in-out': t['--ease-in-out'],
      '--dsw-font-family': t['--vscode-font-family'], '--ds-font-family-code': t['--vscode-editor-font-family'],
      '--dsw-font-markdown-code-font-family': t['--vscode-editor-font-family'],
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
  const embedStyle = document.createElement('style');
  embedStyle.textContent = [
    'html[data-placement="settings"] #root > * {',
    '  height: auto; min-height: 0; max-height: none; overflow: visible;',
    '  width: 100%; max-width: none; margin: 0; padding: 0; align-items: stretch;',
    '}',
    'html[data-placement="settings"] #root > * > * { max-width: none; }',
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
    if (placement === 'background') { root.render(null); return; }
    if (options.settingsForm) {
      root.render(React.createElement(SettingsForm, { scope, schema: settingsSchema, locale, error: settingsError }));
      return;
    }
    const cards = slotContext.live().filter((entry) => selectedSlot.slotName
      ? entry.descriptor.name === selectedSlot.slotName && (selectedSlot.slotKey === undefined || String(entry.descriptor.key ?? entry.descriptor.id ?? '') === selectedSlot.slotKey)
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
    // 官方组件用 <use href="#dsw-icon-…"> 引用文档 sprite；图形仍取公共 SDK 的原始导出。
    // 每份隔离文档独立安装一次，不借用宿主 DOM，也不重绘或替换上游组件。
    const symbols = modules['@deepseek-ai/dsh-client-ui-primitives']?.ICON_SYMBOLS;
    if (symbols) {
      const ns = 'http://www.w3.org/2000/svg', sprite = document.createElementNS(ns, 'svg');
      sprite.setAttribute('aria-hidden', 'true');
      sprite.style.cssText = 'position:absolute;width:0;height:0;overflow:hidden;pointer-events:none';
      for (const [id, source] of Object.entries(symbols)) {
        const svg = new DOMParser().parseFromString(source, 'image/svg+xml').documentElement;
        const symbol = document.createElementNS(ns, 'symbol');
        for (const attribute of svg.attributes) if (attribute.name !== 'xmlns') symbol.setAttribute(attribute.name, attribute.value);
        symbol.id = id;
        for (const child of svg.childNodes) symbol.appendChild(document.importNode(child, true));
        sprite.appendChild(symbol);
      }
      document.body.prepend(sprite);
    }
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
    if (event.source !== parent || !m || m.channel !== CHANNEL) return;
    if (m.type !== 'init' && m.instanceId !== instanceId) return;
    if (m.type === 'init') {
      instanceId = m.instanceId;
      if (initialized) return;
      initialized = true;
      selectView(m);
      lastReported = -1;
      locale = m.locale ?? locale;
      slotContext.setLocale(locale);
      if (m.theme) applyTheme(m.theme);
      if (options.settingsForm) render();
      try {
        // 同步 Web Storage 外观只保存界面缓存；实际存储由宿主按插件身份隔离。
        // 必须在上游 apply 之前初始化，typeof sessionStorage 在 opaque origin 上也会抛错。
        const storage = await request({ operation: 'view.storage.read' });
        for (const area of ['local', 'session']) Object.defineProperty(window, `${area}Storage`, {
          configurable: true,
          value: createWebStorage(storage[area], (mutation) => {
            void request({ operation: 'view.storage.write', area, mutation }).catch((error) => console.error('Plugin UI storage failed:', error));
          }),
        });
        settingsSchema = await refreshSettings();
        if (sessionComposer) await composer.api.refresh();
        if (!options.settingsForm) await loadUpstream(options.clientScript ?? 'client.js');
        render();
      } catch (error) {
        if (options.settingsForm) { settingsError = String(error?.message ?? error); render(); }
        else root.render(React.createElement('div', { style: { padding: 16, color: 'crimson' } }, `界面加载失败：${error?.message ?? error}`));
      }
    } else if (m.type === 'context') {
      if (m.placement) { selectView(m); applyPlacement(); render(); }
      if (m.locale && m.locale !== locale) { locale = m.locale; slotContext.setLocale(locale); render(); }
      if (m.theme) applyTheme(m.theme);
    } else if (m.type === 'composer') {
      composer.update(m.snapshot);
    } else if (m.type === 'response') {
      transport.receive(m);
    }
  });
}
