// 原包注册、优先级、子槽位声明与卸载只由上游纯 SlotCore 维护。
// Magi 只声明宿主边界并管理文档 effect；页面位置及业务事实不归注册表。
import { SlotCore } from '@deepseek-ai/dsh-client-ui-slots';

// kind/scope 核对 DSH 0.2.2-rc.1 各 UI 包的公开 SlotMap；位置处置由 Magi
// 拥有，两者集中在同一合同里。声明不等于挂载，未接入位置仍阻止空白交付。
// mounted 条目的顺序也是多个页面入口的取用顺序，优先新设置入口。
const SLOT_CONTRACTS = {
  'settings.section': { kind: 'list', scope: 'root', disposition: 'mounted', surface: 'settings' },
  'settings.section.icon': { kind: 'list', scope: 'root', disposition: 'alternate' },
  'plugins.bundle.config': { kind: 'keyed', scope: 'root', disposition: 'mounted', surface: 'settings' },
  'plugins.row.config': { kind: 'keyed', scope: 'root', disposition: 'alternate' },
  'settings.plugins.tab': { kind: 'list', scope: 'root', disposition: 'mounted', surface: 'settings' },
  'plugins.tab': { kind: 'list', scope: 'root', disposition: 'mounted', surface: 'settings' },
  'main': { kind: 'keyed', scope: 'root', disposition: 'mounted' },
  'sidebar.right.pane.tab': { kind: 'keyed', scope: 'session', disposition: 'mounted' },
  'sidebar.panellist': { kind: 'list', scope: 'root', disposition: 'alternate' },
  'sidebar.right.pane.tab.title': { kind: 'keyed', scope: 'session', disposition: 'alternate' },
  'sidebar.right.tab.guide.entry': { kind: 'keyed', scope: 'session', disposition: 'alternate' },
  'sidebar.footer.action': { kind: 'list', scope: 'root', disposition: 'alternate' },
  'conversation.input.left': { kind: 'list', scope: 'session', disposition: 'mounted' },
  'conversation.input.right': { kind: 'list', scope: 'session', disposition: 'mounted' },
  'conversation.chat.node': { kind: 'keyed', scope: 'session', disposition: 'blocked', reason: '替换原生消息渲染器' },
  'conversation.session.header.actions': { kind: 'list', scope: 'session', disposition: 'unmounted' },
  'conversation.chat.turnTail': { kind: 'list', scope: 'session', disposition: 'unmounted' },
  'conversation.input.dock': { kind: 'list', scope: 'session', disposition: 'blocked', reason: '接管原生输入区' },
};

export function slotDisposition(name, key) {
  return SLOT_CONTRACTS[name]?.disposition ?? 'unmounted';
}
export function assertSlotAllowed(name) {
  const contract = SLOT_CONTRACTS[name];
  if (contract?.disposition === 'blocked') {
    throw new Error(`PLUGIN_UI_BOUNDARY_CONFLICT：该插件要求${contract.reason}（${name}），与 Magi 的详细／摘要展示和原生交互冲突，不能安装或启用。Magi 不会自动改写为另一种功能，也不提供覆盖原生界面的授权。`);
  }
}
export const MOUNT_PRIORITY = Object.keys(SLOT_CONTRACTS).filter(name => slotDisposition(name) === 'mounted');
export const SLOT_SURFACE = Object.fromEntries(Object.entries(SLOT_CONTRACTS).filter(([, spec]) => spec.surface).map(([name, spec]) => [name, spec.surface]));
const HOST_SLOTS = Object.fromEntries(Object.entries(SLOT_CONTRACTS).map(([name, { kind, scope }]) => [name, { kind, scope }]));

/**
 * @param {{ scope: object, onChange?: () => void }} options
 *   scope    settingsScope 的实现（getSnapshot / subscribe / set）
 *   onChange 槽位注册或释放后的回调
 */
export function createSlotContext({ scope, connection, composer, theme, onChange = () => {}, onOpenTab }) {
  const core = new SlotCore();
  const names = new Set();
  const releaseRoot = core.register({ name: 'root', children: HOST_SLOTS }, () => null);
  const tabTypes = new Map();
  const catalogs = new Map();
  const effects = new Set();
  let disposed = false;
  let owner;
  const effect = (factory) => {
    if (disposed) throw new Error('插件页面已失效');
    const cleanups = [];
    const release = () => {
      if (!effects.delete(release)) return;
      const errors = [];
      for (const cleanup of cleanups.splice(0).reverse()) try { cleanup(); } catch (error) { errors.push(error); }
      if (errors.length) throw new AggregateError(errors, '插件页面清理失败');
    };
    effects.add(release);
    owner?.push(release);
    const parent = owner; owner = cleanups;
    try {
      const result = factory();
      if (typeof result === 'function') cleanups.push(result);
      else if (result != null) {
        if (typeof result[Symbol.iterator] !== 'function') throw new Error('页面 effect 必须返回同步清理函数或其迭代器');
        for (const cleanup of result) {
          if (typeof cleanup !== 'function') throw new Error('页面 effect 迭代器必须产出清理函数');
          cleanups.push(cleanup);
        }
      }
    } catch (error) { try { release(); } catch (cleanupError) { throw new AggregateError([error, cleanupError], '插件页面 effect 初始化失败'); } throw error; } finally { owner = parent; }
    return release;
  };
  const register = (descriptor, component) => effect(() => {
    // 安装记录与运行文档共用同一准入；延迟注册也不能接管原生控件。
    assertSlotAllowed(descriptor.name);
    // names 只是枚举索引；存活身份及所有条目始终向同一个 core 查询。
    return core.register(descriptor, component);
  });
  const inject = (name, factory) => effect(() => {
    let active, epoch;
    let stopped = false;
    const reconcile = () => {
      if (stopped) return;
      const next = core.declarationEpoch(name);
      if (epoch === next) return;
      epoch = next;
      const previous = active; active = undefined; previous?.();
      if (core.specDynamic(name)) active = effect(factory);
    };
    // 声明通知发生在 core.register 返回 disposer 之前。延迟注入失败不能
    // 从通知里抛穿注册事务，使已提交父条目失去它的释放句柄；沿上游服务
    // 的合同停止该注入，下一微任务报告原错误，父注册仍可正常释放。
    const stop = () => { stopped = true; off(); const previous = active; active = undefined; previous?.(); };
    const off = core.subscribeDeclaration(name, () => {
      try { reconcile(); }
      catch (error) { stop(); queueMicrotask(() => { throw error; }); }
    });
    try { reconcile(); } catch (error) { off(); active?.(); throw error; }
    return stop;
  });
  const offMutation = core.onMutate(name => { names.add(name); if (!disposed) onChange(); });
  const localeListeners = new Set();
  let locale = 'zh-CN';

  const configForms = {
    get: () => scope,
    whileServed(candidates, register) {
      const cleanup = register(new Set(candidates));
      return () => { if (typeof cleanup === 'function') cleanup(); };
    },
  };

  const ctx = {
    configForms,
    connection,
    ...(theme ? { theme: theme.service, on(name, listener) {
      if (name !== 'theme/change') throw new Error(`页面事件尚未接入：${name}`);
      return effect(() => theme.subscribe(listener));
    } } : {}),
    slots: {
      register, inject,
      entries: (name) => core.entries(name),
      entriesOfSlot: (name) => core.entriesOfSlot(name),
      subscribe: (name, listener) => effect(() => core.subscribe(name, listener)),
      getVersion: (name) => core.getVersion(name),
      spec: (name) => core.specDynamic(name),
      isLive: (entry) => core.isLive(entry),
    },
    locale: {
      getSnapshot: () => locale,
      subscribe(listener) { localeListeners.add(listener); return () => localeListeners.delete(listener); },
      register(namespace, packs) { catalogs.set(namespace, packs); return () => catalogs.delete(namespace); },
      bind(namespace) {
        return (key, vars) => {
          const packs = catalogs.get(namespace) ?? {};
          const pack = packs[locale] ?? packs[locale.split('-')[0]] ?? packs.zh ?? packs.en ?? {};
          const fallback = packs.en ?? packs.zh ?? {};
          let text = pack[key] ?? fallback[key] ?? key;
          if (vars) for (const [name, value] of Object.entries(vars)) text = text.replaceAll(`{${name}}`, String(value));
          return text;
        };
      },
    },
    effect,
    // 浏览器端上游常把 slots 作为 layout/configForms 等服务的子服务注入。
    // 这些服务不是第二套状态源：这里只暴露同一份槽位注册表与设置快照，
    // 其他 DSH 外壳服务保持缺席，让插件按其原有可选分支降级。
    inject(names, callback) {
      const requested = Array.isArray(names) ? names : [names];
      const services = {
        slots: ctx.slots,
        configForms,
        locale: ctx.locale,
        layout: { slots: ctx.slots },
        sidebarRight: ctx.sidebarRight,
        sidebarRightTabs: ctx.sidebarRightTabs,
        connection,
        ...(theme ? { theme: theme.service } : {}),
        ...(composer ? { sessions: composer.sessions, conversation: composer.conversation } : {}),
      };
      if (requested.some((name) => services[name] === undefined)) return;
      return callback({ ...ctx, ...services, get(name) { return services[name]; } });
    },
    get(name) { return ctx[name]; },
  };
  if (composer) {
    ctx.sessions = composer.sessions;
    ctx.conversation = composer.conversation;
  }
  // 页签类型是界面注册资料；仅记录插件自身声明，业务事实与安装状态仍来自 daemon。
  ctx.sidebarRightTabs = {
    register(definition) {
      if (!definition?.id || !definition?.kind || tabTypes.has(definition.kind)) throw new Error('右栏页签类型无效或重复');
      tabTypes.set(definition.kind, definition);
      return () => tabTypes.delete(definition.kind);
    },
  };
  ctx.sidebarRight = {
    openTab(kind) {
      const type = tabTypes.get(kind);
      if (!type) throw new Error(`右栏页签未注册：${kind}`);
      if (!onOpenTab) throw new Error('当前宿主不提供右栏导航');
      return onOpenTab(type.id);
    },
  };

  return {
    ctx,
    setLocale(next) { if (next !== locale) { locale = next; for (const listener of [...localeListeners]) listener(); } },
    tabTypes: () => [...tabTypes.values()],
    /** 当前仍然有效的注册。 */
    live() {
      return [...names].flatMap(name => core.entriesOfSlot(name).map(entry => ({
        descriptor: { name, ...entry.options, ...Object.fromEntries(['inject', 'children', 'store', 'locale', 'registrant'].filter(key => entry[key] !== undefined).map(key => [key, entry[key]])) },
        component: entry.component,
      })));
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      const errors = [];
      for (const cleanup of [...effects].reverse()) try { cleanup(); } catch (error) { errors.push(error); }
      offMutation(); releaseRoot(); tabTypes.clear(); localeListeners.clear(); catalogs.clear();
      if (errors.length) throw new AggregateError(errors, '插件页面清理失败');
    },
  };
}
