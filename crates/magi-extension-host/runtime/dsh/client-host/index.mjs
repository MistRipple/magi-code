// Native DSH 0.2.2-rc.3 client page host entry.
// Executes original `./client` bundles unmodified in a real Cordis Context with locked
// official renderer, settings mirror, Typert client gateway, and Global Standard Kit.

import { loadedDefinitions, loadListeners, requireModule } from './loader-setup.mjs';
import '@deepseek-ai/dsh-client-connection/client';
import '@deepseek-ai/dsh-api-gateway/client';
import '@deepseek-ai/dsh-api-remotes/client';
import '@deepseek-ai/dsh-api-remote-access/client';
import '@deepseek-ai/dsh-client-file-upload/client';
import '@deepseek-ai/dsh-api-session-controller/client';
import '@deepseek-ai/dsh-client-resources/client';
import '@deepseek-ai/dsh-client-ui-renderer/client';
import '@deepseek-ai/dsh-client-ui-session/client';
import '@deepseek-ai/dsh-client-ui-settings/client';
import './shell.css';

import * as Cordis from '@deepseek-ai/cordis';
import { REMOTE_ACCESS_SNAPSHOT_KEY } from '@deepseek-ai/dsh-api-remote-access/src/snapshot.ts';
import * as ClientStore from '@deepseek-ai/dsh-client-store';
import { ROOT_CHILD_SLOT_SPECS, summarizePluginSlots } from './slot-catalog.mjs';
import {
  installClientTypertRegistry,
  installCommandUiService,
  installGlobalStandardKit,
  installLocaleService,
} from './standard-kit.mjs';
import { applyThemeToDocument } from './theme.mjs';
import { createHostBridge } from './transport.mjs';
import { MagiRootShell } from './root-shell.tsx';
import { installRightPane } from './right-pane.mjs';

const CORE_CLIENT_MODULE_IDS = new Set([
  '@deepseek-ai/dsh-client-connection',
  '@deepseek-ai/dsh-api-gateway',
  '@deepseek-ai/dsh-api-remotes',
  '@deepseek-ai/dsh-api-remote-access',
  '@deepseek-ai/dsh-client-file-upload',
  '@deepseek-ai/dsh-api-session-controller',
  '@deepseek-ai/dsh-client-resources',
  '@deepseek-ai/dsh-client-ui-renderer',
  '@deepseek-ai/dsh-client-ui-session',
  '@deepseek-ai/dsh-client-ui-settings',
]);

const hostViewStore = ClientStore.createSnapshotStore({
  pluginId: '',
  placement: 'settings',
  viewId: null,
  sessionId: null,
  paneTab: null,
  settingsInsets: { left: 0, right: 0 },
});
const mainSessionRefStore = ClientStore.createSnapshotStore(undefined);

let booted = false;
let localeServiceRef = null;
let standardKitRef = null;
let reportSlotsRef = null;
let rightPaneRef = null;
let sessionsRef = null;
let mainViewRef = null;
let desiredMainSessionId = null;
let refreshRequestedForId = null;
let syncingMainSession = false;

function syncMainSession(nextSessionId) {
  desiredMainSessionId = typeof nextSessionId === 'string' && nextSessionId.length > 0 ? nextSessionId : null;
  if (!sessionsRef || syncingMainSession) return;
  syncingMainSession = true;
  try {
    if (mainViewRef?.sessionId === desiredMainSessionId) return;
    if (mainViewRef) {
      const prev = mainViewRef;
      mainViewRef = null;
      mainSessionRefStore.set(undefined);
      try { prev.release(); } catch { /* ignore */ }
    }
    if (!desiredMainSessionId) {
      refreshRequestedForId = null;
      return;
    }
    const snap = sessionsRef.list.getSnapshot();
    if (!snap?.byId?.[desiredMainSessionId]) {
      // Wait for the authoritative `session/list` catalog before calling `retain`,
      // because the locked controller rejects retaining an uncatalogued session id.
      if (refreshRequestedForId !== desiredMainSessionId) {
        refreshRequestedForId = desiredMainSessionId;
        void sessionsRef.refresh().catch(() => {});
      }
      return;
    }
    refreshRequestedForId = null;
    try {
      const ref = sessionsRef.retain(desiredMainSessionId, { source: 'mainView' });
      ref.ready.catch(() => {});
      mainViewRef = ref;
      mainSessionRefStore.set(ref);
    } catch (err) {
      console.error('[dsh-client-host] failed to retain main session:', err);
    }
  } finally {
    syncingMainSession = false;
  }
}

const bridge = createHostBridge({
  onClose: () => {
    syncMainSession(null);
    return rightPaneRef?.closeDocument();
  },
  onInit(initData) {
    applyThemeToDocument(initData.theme);
    hostViewStore.set({
      pluginId: typeof initData.pluginId === 'string' ? initData.pluginId : '',
      placement: typeof initData.placement === 'string' ? initData.placement : 'settings',
      viewId: typeof initData.viewId === 'string' ? initData.viewId : null,
      sessionId: typeof initData.sessionId === 'string' ? initData.sessionId : null,
      paneTab: initData.paneTab ?? null,
      settingsInsets: initData.settingsInsets ?? { left: 0, right: 0 },
    });
    rightPaneRef?.update(initData.paneTab);
    if (localeServiceRef && typeof initData.locale === 'string') {
      localeServiceRef.setLocale(initData.locale);
    }
    standardKitRef?.updateHostFacts({
      placement: initData.placement,
      activePanelId: initData.viewId ?? undefined,
    });
    syncMainSession(initData.sessionId);
    if (!booted) {
      booted = true;
      void bootCordisClientApp(initData).catch(err => {
        console.error('[dsh-client-host] failed to boot client app:', err);
      });
    } else {
      reportSlotsRef?.();
    }
  },
  onContext(contextData) {
    if (contextData.theme) applyThemeToDocument(contextData.theme);
    const prev = hostViewStore.getSnapshot();
    const nextSessionId = contextData.sessionId !== undefined ? contextData.sessionId : prev.sessionId;
    hostViewStore.set({
      pluginId: prev.pluginId,
      placement: typeof contextData.placement === 'string' ? contextData.placement : prev.placement,
      viewId: contextData.viewId !== undefined ? contextData.viewId : prev.viewId,
      sessionId: nextSessionId,
      paneTab: contextData.paneTab ?? null,
      settingsInsets: contextData.settingsInsets ?? prev.settingsInsets,
    });
    rightPaneRef?.update(contextData.paneTab);
    if (localeServiceRef && typeof contextData.locale === 'string') {
      localeServiceRef.setLocale(contextData.locale);
    }
    standardKitRef?.updateHostFacts({
      placement: contextData.placement,
      activePanelId: contextData.viewId ?? undefined,
    });
    syncMainSession(nextSessionId);
  },
});

bridge.installBrowserHooks();

function unwrapCordisPlugin(mod) {
  if (!mod) return mod;
  if (typeof mod === 'function' || typeof mod.apply === 'function') return mod;
  if (
    mod.default &&
    (typeof mod.default === 'function' ||
      (typeof mod.default === 'object' && typeof mod.default.apply === 'function'))
  ) {
    return mod.default;
  }
  return mod;
}

async function bootCordisClientApp(initData) {
  await bridge.hydrateStorage();
  // The daemon admits a page before it is served, so every page here is the owner; the access stream
  // opened by the real access client confirms or replaces this opening snapshot.
  Reflect.set(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY, Object.freeze({ type: 'baseline', owner: true }));
  const ctx = new Cordis.Context();
  installClientTypertRegistry(ctx);

  const localeService = installLocaleService(ctx, initData.locale || 'zh-CN');
  localeServiceRef = localeService;

  installCommandUiService(ctx);

  // Mount locked official DSH client plugins in dependency order.
  // Note: dsh-client-connection -> dsh-api-gateway -> dsh-api-remotes form a service cycle/chain
  // (`connection` waits on `remote` from `dsh-api-gateway`), so register all core plugins first, then await!
  const coreFibers = [
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-connection'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-api-gateway'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-api-remotes'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-api-remote-access'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-file-upload'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-api-session-controller'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-resources'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-ui-renderer'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-ui-session'))),
    ctx.plugin(unwrapCordisPlugin(requireModule('@deepseek-ai/dsh-client-ui-settings'))),
  ];
  await Promise.all(coreFibers.map(f => f.await()));
  // `dsh-api-remotes` mounts ~30 Typert namespaces asynchronously before `remote.settings`,
  // `remote.session`, `remote.commands` and `remote.subagents` unlock `configForms`, `sessions`
  // and `uiSession`. Wait until all core client services are live before mounting user plugins.
  await new Promise(resolve => {
    let settled = false;
    const waitFiber = ctx.inject(['uiRenderer', 'configForms', 'sessions', 'uiSession'], () => {
      if (settled) return;
      settled = true;
      queueMicrotask(() => {
        waitFiber?.dispose?.();
        resolve();
      });
    });
  });

  sessionsRef = ctx.get('sessions');
  sessionsRef.list.subscribe(() => {
    syncMainSession(desiredMainSessionId);
  });
  void sessionsRef.refresh().catch(() => {});
  syncMainSession(desiredMainSessionId);

  ctx.slots.installLocale(localeService);
  const standardKit = installGlobalStandardKit(ctx);
  const rightPane = installRightPane(ctx, bridge);
  rightPaneRef = rightPane;
  rightPane.update(hostViewStore.getSnapshot().paneTab);
  standardKitRef = standardKit;
  standardKit.updateHostFacts({
    placement: initData.placement,
    activePanelId: initData.viewId ?? undefined,
  });

  // Register root host shell declaring the complete canonical DSH slot catalog.
  ctx.slots.register(
    {
      name: 'root',
      children: ROOT_CHILD_SLOT_SPECS,
      inject: () => ({
        hooks: {
          hostView: hostViewStore,
          mainSessionRef: mainSessionRefStore,
        },
        slotsRegistry: ctx.slots,
        localeService,
        configForms: ctx.get('configForms'),
        bridge,
        rightPane,
      }),
    },
    MagiRootShell,
  );

  let initialBootDone = false;
  let reportScheduled = false;
  let observedRightPane = false;
  const scheduleSlotReport = () => {
    if (!initialBootDone || reportScheduled) return;
    reportScheduled = true;
    queueMicrotask(() => {
      reportScheduled = false;
      const view = hostViewStore.getSnapshot();
      if (view.paneTab) return;
      const rightPaneTabs = rightPane.describeTabs();
      observedRightPane ||= rightPaneTabs.length > 0;
      const summary = summarizePluginSlots(ctx.slots, view.pluginId, rightPaneTabs);
      // A fresh client document can report root slots before nested
      // registrations run. Only an observed ledger can report their removal.
      if (observedRightPane) summary.rightPaneTabs = rightPaneTabs;
      bridge.reportSlots(summary);
    });
  };
  reportSlotsRef = scheduleSlotReport;
  ctx.on('slots/changed', scheduleSlotReport);
  rightPane.subscribe(scheduleSlotReport);
  localeService.subscribe(scheduleSlotReport);

  // Mount every non-core client plugin bundle loaded via window.__ModuleLoader__.load(...)
  const mountedPlugins = new Set();
  const initialPluginPromises = [];
  const mountPluginById = id => {
    if (CORE_CLIENT_MODULE_IDS.has(id) || mountedPlugins.has(id)) return;
    mountedPlugins.add(id);
    try {
      const mod = requireModule(id);
      const fiber = ctx.plugin(unwrapCordisPlugin(mod));
      const p = fiber.await().then(scheduleSlotReport).catch(err => {
        console.error(`[dsh-client-host] client plugin "${id}" startup failed:`, err);
      });
      if (!initialBootDone) initialPluginPromises.push(p);
    } catch (err) {
      console.error(`[dsh-client-host] failed to mount client plugin "${id}":`, err);
    }
  };

  for (const id of loadedDefinitions.keys()) {
    mountPluginById(id);
  }
  loadListeners.add(mountPluginById);
  await Promise.allSettled(initialPluginPromises);
  await new Promise(r => queueMicrotask(r));

  const rootEl =
    document.getElementById('root') ??
    (() => {
      const el = document.createElement('div');
      el.id = 'root';
      document.body.appendChild(el);
      return el;
    })();

  initialBootDone = true;
  scheduleSlotReport();
  ctx.get('uiRenderer')?.mount(rootEl);
}
