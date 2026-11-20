import { createSnapshotStore } from '@deepseek-ai/dsh-client-store';
import { SidebarRightTabRegistry } from '@deepseek-ai/dsh-client-ui-sidebar-right/src/client/tab-registry.ts';

const BODY_SLOT = 'sidebar.right.pane.tab';

export function installRightPane(ctx, bridge) {
  const registry = new SidebarRightTabRegistry(ctx);
  const mounted = createSnapshotStore(undefined);
  const closeHandlers = new Map();
  let current;

  function navigate(definition, address, params) {
    if (!current?.visible || !definition) return;
    void bridge.request({
      operation: 'view.open',
      viewId: definition.id,
      address,
      title: definition.title(address),
      params,
    }).catch(error => console.error('[dsh-client-host] Right pane navigation failed:', error));
  }

  function openTab(kind, options) {
    navigate(registry.get(kind), '', options?.params);
  }

  function openResource(address, options) {
    navigate(registry.candidates(address)[0], address, options?.params);
  }

  function act(action, tabId) {
    if (!current?.visible) return;
    void bridge.request({ operation: action, tabId })
      .catch(error => console.error('[dsh-client-host] Right pane action failed:', error));
  }

  const close = tabId => act('view.close', tabId);
  const actions = Object.freeze({
    openTab,
    openResource,
    close: () => { if (current) close(current.id); },
  });

  ctx.reflect.provide('sidebarRightTabs', registry);
  ctx.reflect.provide('sidebarRight', Object.freeze({
    openTab,
    openResource,
    close,
    focus: tabId => act('view.focus', tabId),
    mounted,
    isExpanded: () => current?.expanded === true,
    active: () => current?.visible ? current.id : undefined,
    registerCloseHandler(kind, handler) {
      const handlers = closeHandlers.get(kind) ?? new Set();
      handlers.add(handler);
      closeHandlers.set(kind, handlers);
      return () => {
        handlers.delete(handler);
        if (!handlers.size) closeHandlers.delete(kind);
      };
    },
  }));

  const lifetime = new AbortController();
  let disposed = false;
  async function closeDocument() {
    if (disposed) return;
    disposed = true;
    if (current) {
      const definition = registry.entries().find(row => row.id === current.viewId);
      for (const handler of closeHandlers.get(definition?.kind) ?? []) {
        try { handler(current.sessionId, { ...current, kind: definition.kind }); }
        catch (error) { console.error('[dsh-client-host] Right pane close handler failed:', error); }
      }
    }
    lifetime.abort();
    await bridge.flushStorage();
  }
  addEventListener('pagehide', () => { void closeDocument(); }, { once: true });

  return {
    update(tab) {
      current = tab;
      mounted.set(tab?.visible ? tab.sessionId : undefined);
    },
    closeDocument,
    useTabInfo(record) {
      const definition = registry.entries().find(row => row.id === record.viewId);
      const address = record.navigation.address || `sidebar://${definition?.kind}`;
      return {
        sidebar: { expanded: record.expanded, fullscreen: record.overlay },
        panel: { id: 'rightPane', isFloating: false },
        tab: {
          id: record.id,
          kind: definition?.kind,
          title: record.title,
          contentId: address,
          visible: record.visible,
          navigation: { ...record.navigation, address },
          signal: lifetime.signal,
          actions,
        },
      };
    },
    subscribe: listener => registry.subscribe(listener),
    getEntries: () => registry.entries(),
    describeTabs() {
      const bodyKeys = new Set(ctx.slots.entries(BODY_SLOT).map(entry => entry.options.key));
      const guide = registry.guide();
      return registry.entries().filter(tab => bodyKeys.has(tab.id)).map(tab => {
        const entry = guide.find(item => item.providerId === tab.id);
        return {
          key: tab.id,
          title: entry?.title() ?? tab.title(`sidebar://${tab.kind}`),
          ...(entry ? { launcherOrder: entry.order } : {}),
        };
      });
    },
  };
}
