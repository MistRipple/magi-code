// DSH 浏览器端上下文（ctx）的最小实现：只含上游界面入口 `apply(ctx)` 用到的成员。
// 不依赖 DOM：浏览器里的页面壳和构建期的记录器共用这一份，所以“构建期记录到的槽位”就是“运行期收到的槽位”。
//
// 范围（其余成员不存在，调用即 TypeError，而不是静默吞掉）：
//   ctx.slots.inject / ctx.slots.register   收集槽位注册
//   ctx.locale.register / ctx.locale.bind   上游自带语言包
//   ctx.effect                              登记清理
//   ctx.get('configForms')                  设置读写入口（背后由调用方提供 scope）
//   ctx.get(其他)                           undefined —— 上游约定“服务缺席即降级”

/** 每个 DSH 槽位在 Magi 里的去向。未登记的槽位一律视为“未挂载”并进入兼容报告。 */
export const SLOT_POLICY = {
  // 设置面板里的一节（DSH 上游 5.19 起的配置入口，列表槽位，id 由插件自己拥有）：Magi 的设置视图整页渲染它。
  'settings.section': 'mounted',
  // 该节的导航图标：Magi 的导航图标取自包清单声明的 icon，不渲染这份注册。
  'settings.section.icon': 'alternate',
  // 插件配置卡片（较早的配置入口）：Magi 的设置视图整页渲染它。
  'plugins.bundle.config': 'mounted',
  // 同一张卡片在 DSH 另一种行布局里的注册；Magi 只有一种页面，取 bundle.config 那份。
  'plugins.row.config': 'alternate',
  // DSH 的完整页面与右栏页签都可以由 Magi 的视图容器承载。它们仍在
  // 上游 iframe 中原样运行，只把入口位置映射到 Magi 的 main/rightPane。
  'main': 'mounted',
  'sidebar.right.pane.tab': 'mounted',
  'settings.plugins.tab': 'mounted',
  'plugins.tab': 'mounted',
  'sidebar.panellist': 'alternate',
  'sidebar.right.pane.tab.title': 'alternate',
  'sidebar.right.tab.guide.entry': 'alternate',
  'sidebar.footer.action': 'alternate',
};

export function slotDisposition(name) {
  return SLOT_POLICY[name] ?? 'unmounted';
}

/** 多个可渲染槽位同时存在时的取用顺序：新入口优先。 */
export const MOUNT_PRIORITY = ['settings.section', 'plugins.bundle.config', 'settings.plugins.tab', 'plugins.tab', 'main', 'sidebar.right.pane.tab'];
/** 各槽位给上游组件的 surface：`settings.section` 的外壳（标题、关闭）由宿主拥有，组件只渲染内容。 */
export const SLOT_SURFACE = { 'settings.section': 'settings' };

/**
 * @param {{ scope: object, onChange?: () => void }} options
 *   scope    settingsScope 的实现（getSnapshot / subscribe / set）
 *   onChange 槽位注册或释放后的回调
 */
export function createSlotContext({ scope, connection, onChange = () => {} }) {
  const registered = [];
  const catalogs = new Map();
  const effects = [];
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
    slots: {
      inject(_name, factory) { return factory(); },
      register(descriptor, component) {
        const entry = { descriptor, component, disposed: false };
        registered.push(entry);
        onChange();
        return () => { entry.disposed = true; onChange(); };
      },
    },
    locale: {
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
    effect(fn) { effects.push(fn()); },
    // 浏览器端上游常把 slots 作为 layout/configForms 等服务的子服务注入。
    // 这些服务不是第二套状态源：这里只暴露同一份槽位收集器与设置快照，
    // 其他 DSH 外壳服务保持缺席，让插件按其原有可选分支降级。
    inject(names, callback) {
      const requested = Array.isArray(names) ? names : [names];
      const services = {
        slots: ctx.slots,
        configForms,
        locale: ctx.locale,
        layout: { slots: ctx.slots },
      };
      if (requested.some((name) => !Object.hasOwn(services, name))) return;
      return callback({ get(name) { return services[name]; }, slots: ctx.slots, configForms, locale: ctx.locale });
    },
    get(name) { return name === 'configForms' ? configForms : name === 'connection' ? connection : undefined; },
  };

  return {
    ctx,
    setLocale(next) { locale = next; },
    /** 当前仍然有效的注册。 */
    live() { return registered.filter((entry) => !entry.disposed); },
    dispose() { for (const cleanup of effects.splice(0)) if (typeof cleanup === 'function') cleanup(); },
  };
}
