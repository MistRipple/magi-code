// Client services and shell-owned global standard kit members (`panelInfo`, `workspaceAccess`).
// Session list, status, retain info, access, and session/session-maybe scopes come from the locked
// `@deepseek-ai/dsh-api-session-controller/client` and `@deepseek-ai/dsh-client-ui-session/client` packages.
import { Service } from '@deepseek-ai/cordis';
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store';

function panelInfo(placement, activePanelId) {
  const id = placement === 'page' && typeof activePanelId === 'string' && activePanelId.length > 0
    ? activePanelId : undefined;
  return Object.freeze({ activePanelId: id, active: id !== undefined });
}

export function installClientTypertRegistry(ctx) {
  const localMap = new Map();
  const seenLocal = new Set();
  const remoteContributions = new Set();
  const lookupProviders = new Map();
  const lookupDefinitions = new Map();
  const lookupOverrides = new Map();
  const hostContexts = new Map();
  const hostContextOverrides = new Map();
  const clientContexts = new Map();
  const resourceMap = new Map();

  const localListeners = new Set();
  const remoteListeners = new Set();
  const lookupListeners = new Set();
  const contextListeners = new Set();
  const resourceListeners = new Set();

  const notify = (listeners, event) => {
    for (const fn of [...listeners]) {
      try { fn(event); } catch (err) { console.error('[typert] listener threw:', err); }
    }
  };
  const makeSub = listeners => fn => {
    listeners.add(fn);
    return () => { listeners.delete(fn); };
  };

  const registry = {
    local: {
      get: endpoint => localMap.get(endpoint),
      hasSeen: endpoint => seenLocal.has(endpoint),
      list: () => [...localMap.values()],
      subscribe: makeSub(localListeners),
    },
    remotes: {
      register(contribution) {
        remoteContributions.add(contribution);
        notify(remoteListeners, { kind: 'remote', key: contribution.package });
        return async () => {
          if (remoteContributions.delete(contribution)) {
            notify(remoteListeners, { kind: 'remote', key: contribution.package });
          }
        };
      },
      get(endpoint) {
        for (const item of remoteContributions) {
          const found = item.descriptors.find(d => `${d.namespace}/${d.method}` === endpoint);
          if (found) return found;
        }
        return undefined;
      },
      list: () => [...remoteContributions].flatMap(item => [...item.descriptors]),
      subscribe: makeSub(remoteListeners),
    },
    lookups: {
      register(key, provider) {
        lookupProviders.set(key, provider);
        lookupDefinitions.set(key, Object.freeze({
          key,
          parameter: provider.parameter,
          wire: provider.wire,
          hostTypeSymbol: provider.hostTypeSymbol,
          wireTypeSymbol: provider.wireTypeSymbol,
        }));
        notify(lookupListeners, { kind: 'lookup', key });
        return async () => {
          if (lookupProviders.get(key) === provider) {
            lookupProviders.delete(key);
            notify(lookupListeners, { kind: 'lookup', key });
          }
        };
      },
      configure(key, resolver) {
        lookupOverrides.set(key, resolver);
        notify(lookupListeners, { kind: 'lookup', key });
        return async () => {
          if (lookupOverrides.get(key) === resolver) {
            lookupOverrides.delete(key);
            notify(lookupListeners, { kind: 'lookup', key });
          }
        };
      },
      get(key) {
        const base = lookupProviders.get(key);
        if (!base) return undefined;
        const override = lookupOverrides.get(key);
        return override ? { ...base, resolve: override } : base;
      },
      definitions: () => [...lookupDefinitions.values()],
      keys: () => [...lookupProviders.keys()],
      subscribe: makeSub(lookupListeners),
    },
    contexts: {
      registerHost(key, adapter) {
        hostContexts.set(key, adapter);
        notify(contextListeners, { kind: 'host-context', key });
        return async () => {
          if (hostContexts.get(key) === adapter) {
            hostContexts.delete(key);
            notify(contextListeners, { kind: 'host-context', key });
          }
        };
      },
      configureHost(key, resolver) {
        hostContextOverrides.set(key, resolver);
        notify(contextListeners, { kind: 'host-context', key });
        return async () => {
          if (hostContextOverrides.get(key) === resolver) {
            hostContextOverrides.delete(key);
            notify(contextListeners, { kind: 'host-context', key });
          }
        };
      },
      registerClient(key, adapter) {
        clientContexts.set(key, adapter);
        notify(contextListeners, { kind: 'client-context', key });
        return async () => {
          if (clientContexts.get(key) === adapter) {
            clientContexts.delete(key);
            notify(contextListeners, { kind: 'client-context', key });
          }
        };
      },
      getHost(key) {
        const base = hostContexts.get(key);
        if (!base) return undefined;
        const override = hostContextOverrides.get(key);
        return override ? { ...base, resolve: override } : base;
      },
      getClient: key => clientContexts.get(key),
      subscribe: makeSub(contextListeners),
    },
    resources: {
      register(kind, declaration) {
        if (kind === 'global' || resourceMap.has(kind)) {
          throw new Error(`typert resource kind "${kind}" is reserved or already registered`);
        }
        const entry = Object.freeze({ ...declaration, kind });
        resourceMap.set(kind, entry);
        notify(resourceListeners, { kind: 'resource', key: kind });
        return async () => {
          if (resourceMap.get(kind) === entry) {
            resourceMap.delete(kind);
            notify(resourceListeners, { kind: 'resource', key: kind });
          }
        };
      },
      get: kind => resourceMap.get(kind),
      entries: () => [...resourceMap.values()],
      subscribe: makeSub(resourceListeners),
    },
  };

  ctx.provide('typert', true);
  ctx.typert = registry;
  return registry;
}

function normalizeLanguage(locale) {
  if (typeof locale !== 'string' || !locale.trim()) return 'zh';
  const lower = locale.trim().toLowerCase();
  if (lower.startsWith('zh')) return 'zh';
  if (lower.startsWith('en')) return 'en';
  if (lower.startsWith('ja')) return 'ja';
  return lower.split('-')[0] || 'en';
}

export function installLocaleService(ctx, initialLocale = 'zh-CN') {
  const catalogs = new Map();
  const state = createSnapshotStore({
    language: normalizeLanguage(initialLocale),
    locale: initialLocale,
    revision: 1,
  });

  const bump = nextLocale => {
    const cur = state.getSnapshot();
    const locale = nextLocale ?? cur.locale;
    state.set({
      language: normalizeLanguage(locale),
      locale,
      revision: cur.revision + 1,
    });
  };

  const formatTemplate = (template, params) => {
    if (typeof template !== 'string') return String(template ?? '');
    if (!params || typeof params !== 'object') return template;
    return template.replace(/\{([A-Za-z0-9_.-]+)\}/g, (match, key) =>
      Object.hasOwn(params, key) && params[key] !== undefined ? String(params[key]) : match,
    );
  };

  const bind = ns => (key, params) => {
    const { language, locale } = state.getSnapshot();
    const packs = catalogs.get(ns) ?? {};
    const common = catalogs.get('common') ?? {};
    const dict = packs[locale] ?? packs[language] ?? packs.en ?? packs.zh ?? {};
    const fallback = packs.en ?? packs.zh ?? {};
    const commonDict = common[locale] ?? common[language] ?? common.en ?? common.zh ?? {};
    const raw = dict[key] ?? fallback[key] ?? commonDict[key] ?? key;
    return formatTemplate(raw, params);
  };

  const resolveText = value => {
    if (value == null) return '';
    if (typeof value === 'string') return value;
    if (typeof value === 'function') return String(value() ?? '');
    if (typeof value === 'object') {
      const { language, locale } = state.getSnapshot();
      const chosen = value[locale] ?? value[language] ?? value.zh ?? value.en ?? Object.values(value)[0];
      return typeof chosen === 'string' ? chosen : '';
    }
    return String(value);
  };

  class ClientLocaleService extends Service {
    constructor(owner) {
      super(owner, 'locale', true);
    }
    getSnapshot() { return state.getSnapshot(); }
    subscribe(fn) { return state.subscribe(fn); }
    get language() { return state.getSnapshot().language; }
    register(ns, dictionaries) {
      catalogs.set(ns, dictionaries ?? {});
      bump();
      return () => {
        if (catalogs.get(ns) === dictionaries) {
          catalogs.delete(ns);
          bump();
        }
      };
    }
    bind(ns) { return bind(ns); }
    resolveText(value) { return resolveText(value); }
    setLocale(nextLocale) {
      if (typeof nextLocale === 'string' && nextLocale !== state.getSnapshot().locale) {
        bump(nextLocale);
      }
    }
  }

  const service = new ClientLocaleService(ctx);
  return service;
}

export function installCommandUiService(ctx) {
  const contributions = new Map();
  const decorations = new Map();

  class ClientCommandUiService extends Service {
    constructor(owner) {
      super(owner, 'commandUi', true);
    }
    register(contribution) {
      return this.ctx.effect(() => {
        if (contribution?.name) contributions.set(contribution.name, contribution);
        return () => {
          if (contribution?.name && contributions.get(contribution.name) === contribution) {
            contributions.delete(contribution.name);
          }
        };
      }, 'commandUi.register()');
    }
    decorate(decoration) {
      return this.ctx.effect(() => {
        if (decoration?.name) decorations.set(decoration.name, decoration);
        return () => {
          if (decoration?.name && decorations.get(decoration.name) === decoration) {
            decorations.delete(decoration.name);
          }
        };
      }, 'commandUi.decorate()');
    }
    dismiss(_name) {}
  }

  return new ClientCommandUiService(ctx);
}

/**
 * Shell-owned standard kit members (`panelInfo`, `workspaceAccess`) plus dynamic module resolution.
 * Session facts (`sessions`, `sessionStatus`, `sessionRetainInfo`, `sessionAccess`, and the
 * `session` / `session-maybe` scope adapter) are provided by `@deepseek-ai/dsh-client-ui-session/client`.
 */
export function installGlobalStandardKit(ctx) {
  const panelStore = createSnapshotStore(panelInfo('settings', undefined));
  const workspacesStore = createSnapshotStore(Object.freeze([]));
  const contextMenuStore = createSnapshotStore(Object.freeze({
    open: false,
    x: 0,
    y: 0,
    items: Object.freeze([]),
    openMenu(x, y, items) {
      contextMenuStore.set(Object.freeze({
        open: true,
        x: Number(x) || 0,
        y: Number(y) || 0,
        items: Object.freeze(Array.isArray(items) ? [...items] : []),
        openMenu: contextMenuStore.getSnapshot().openMenu,
        closeMenu: contextMenuStore.getSnapshot().closeMenu,
      }));
    },
    closeMenu() {
      const cur = contextMenuStore.getSnapshot();
      if (!cur.open) return;
      contextMenuStore.set(Object.freeze({
        open: false,
        x: 0,
        y: 0,
        items: Object.freeze([]),
        openMenu: cur.openMenu,
        closeMenu: cur.closeMenu,
      }));
    },
  }));

  // Derive real workspace facts from `sessions.list` (single source of truth, never invented).
  const deriveWorkspaces = () => {
    const sessions = ctx.get('sessions');
    const snap = sessions?.list?.getSnapshot?.();
    const list = Array.isArray(snap)
      ? snap
      : snap && typeof snap === 'object' && snap.byId && typeof snap.byId === 'object'
        ? Object.values(snap.byId)
        : [];
    const byId = new Map();
    for (const item of list) {
      const id = item?.workspaceId;
      if (typeof id !== 'string' || !id) continue;
      if (!byId.has(id)) {
        byId.set(id, Object.freeze({
          id,
          name: id,
          cwd: item.cwd ?? undefined,
          sessionCount: 1,
        }));
      } else {
        const prev = byId.get(id);
        byId.set(id, Object.freeze({
          ...prev,
          sessionCount: prev.sessionCount + 1,
        }));
      }
    }
    workspacesStore.set(Object.freeze([...byId.values()]));
  };
  const sessionsService = ctx.get('sessions');
  if (sessionsService?.list?.subscribe) {
    const unsubscribe = sessionsService.list.subscribe(deriveWorkspaces);
    ctx.effect(() => unsubscribe, 'standard-kit: workspaces subscription');
    deriveWorkspaces();
  }

  ctx.reflect.provide('modules', Object.freeze({
    import: async specifier => globalThis.__ModuleLoader__?.require?.(specifier),
  }));

  // Access answers are the real DSH access service's observables for the admitted peer.
  const accessSource = kind => (id, action) => (action === undefined
    ? undefined : ctx.remoteAccess.source(kind, id, action));

  ctx.slots.provideRoot({
    hooks: {
      panelInfo: panelStore,
      workspaces: workspacesStore,
      contextMenu: contextMenuStore,
    },
    keyedHooks: {
      workspaceAccess: accessSource('workspace'),
    },
  });

  return {
    updateHostFacts({ placement, activePanelId }) {
      if (placement !== undefined || activePanelId !== undefined) {
        panelStore.set(panelInfo(placement ?? 'settings', activePanelId));
      }
    },
  };
}
