import * as React from 'react';
import { resolveSlotLabel } from '@deepseek-ai/dsh-client-ui-slots';
import { SegmentedTabs } from '@deepseek-ai/dsh-client-ui-primitives';

interface HostViewSnapshot {
  pluginId: string;
  placement: string;
  sessionId?: string;
  viewId: string | null;
  settingsInsets?: { left: number; right: number };
  paneTab?: { viewId: string } | null;
}

interface RootShellInjectedProps {
  renderSlot: (key: string, owner: Record<string, unknown>, opts?: Record<string, unknown>) => React.ReactNode;
  renderSlotChain?: (key: string, owner: Record<string, unknown>, opts?: Record<string, unknown>) => React.ReactNode;
  SessionProvider: (props: { session?: unknown; empty?: () => React.ReactNode; children: React.ReactNode }) => React.ReactNode;
  useHostView: <T>(selector: (state: HostViewSnapshot) => T) => T;
  useMainSessionRef: <T>(selector: (state: unknown) => T) => T;
  slotsRegistry: {
    entriesOfSlot: (key: string) => ReadonlyArray<{ options: { id?: string; key?: string; order?: number; label?: string | (() => string) } }>;
    subscribe: (key: string, fn: () => void) => () => void;
    getVersion: (key: string) => number;
  };
  localeService: {
    subscribe: (fn: () => void) => () => void;
    getSnapshot: () => { revision: number };
  };
  configForms?: {
    get: (entryId: string) => unknown;
  };
  rightPane: {
    subscribe: (listener: () => void) => () => void;
    getEntries: () => ReadonlyArray<{ id: string }>;
    useTabInfo: (record: NonNullable<HostViewSnapshot['paneTab']>) => unknown;
  };
  bridge: {
    closeSettings: () => void;
    installPlugin: (spec: unknown) => Promise<unknown>;
    updatePlugin: (packageName: string, version?: string) => Promise<unknown>;
    openPackage: (packageName: string) => void;
    reportHeight: (height: number) => void;
  };
}

function useSlotLedger(
  slotsRegistry: RootShellInjectedProps['slotsRegistry'],
  localeService: RootShellInjectedProps['localeService'],
  keys: readonly string[],
) {
  const subscribe = React.useCallback(
    (onStoreChange: () => void) => {
      const offs = [
        ...keys.map(k => slotsRegistry.subscribe(k, onStoreChange)),
        localeService.subscribe(onStoreChange),
      ];
      return () => {
        for (const off of offs) off();
      };
    },
    [slotsRegistry, localeService, keys],
  );
  const getSnapshot = React.useCallback(
    () => `${localeService.getSnapshot().revision}:${keys.map(k => slotsRegistry.getVersion(k)).join(',')}`,
    [slotsRegistry, localeService, keys],
  );
  return React.useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

const SETTINGS_WATCHED_KEYS = [
  'settings.section',
  'settings.section.icon',
  'settings.plugins.tab',
  'plugins.tab',
  'plugins.bundle.config',
  'plugins.bundle.activation',
  'plugins.row.config',
  'plugins.item',
  'plugins.detail.actions',
  'plugins.detail.badge',
  'plugins.detail.section',
  'plugins.add.actions',
  'main',
  'sidebar.right.pane.tab',
  'sidebar.right.pane.tab.title',
  'sidebar.right.tab.document',
  'sidebar.right.tab.document.action',
] as const;

export function MagiRootShell(props: RootShellInjectedProps) {
  const { renderSlot, SessionProvider, useHostView, useMainSessionRef, slotsRegistry, localeService, configForms, bridge, rightPane } = props;
  const hostView = useHostView(s => s);
  const mainSessionRef = useMainSessionRef(s => s);
  useSlotLedger(slotsRegistry, localeService, SETTINGS_WATCHED_KEYS);
  const tabTypes = React.useSyncExternalStore(rightPane.subscribe, rightPane.getEntries);
  const paneTab = hostView.paneTab;
  const readTabInfo = React.useCallback(() => paneTab && rightPane.useTabInfo(paneTab), [paneTab, rightPane, tabTypes]);

  const containerRef = React.useRef<HTMLDivElement | null>(null);
  const [selectedSectionId, setSelectedSectionId] = React.useState<string | null>(null);
  const [selectedPluginsTabId, setSelectedPluginsTabId] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (hostView.placement !== 'settings') return;
    const node = containerRef.current;
    if (!node) return;
    let frame = 0;
    let lastReported = -1;
    const measure = () => {
      frame = 0;
      const next = Math.ceil(node.getBoundingClientRect().height);
      if (next > 0 && Math.abs(next - lastReported) >= 2) {
        lastReported = next;
        bridge.reportHeight(next);
      }
    };
    const schedule = () => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };
    schedule();
    const ro = new ResizeObserver(schedule);
    ro.observe(node);
    const mo = new MutationObserver(schedule);
    mo.observe(node, { childList: true, subtree: true, attributes: true, characterData: true });
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame);
      ro.disconnect();
      mo.disconnect();
    };
  }, [hostView.placement, bridge]);

  if (hostView.placement === 'page') {
    const mainEntries = slotsRegistry.entriesOfSlot('main');
    const requestedKey = hostView.viewId?.startsWith('main:')
      ? hostView.viewId.slice('main:'.length)
      : hostView.viewId;
    const activeKey =
      (requestedKey && mainEntries.some(e => e.options.key === requestedKey) ? requestedKey : undefined) ??
      mainEntries.find(e => e.options.key === hostView.pluginId)?.options.key ??
      mainEntries[0]?.options.key;

    return (
      <div className="magi-dsh-page-shell" data-testid="dsh-page-shell">
        {activeKey ? renderSlot('main', {}, { entryKey: activeKey }) : null}
      </div>
    );
  }

  if (hostView.placement === 'rightPane') {
    const hasTabTitleSlot = paneTab
      ? slotsRegistry
          .entriesOfSlot('sidebar.right.pane.tab.title')
          .some(e => e.options.key === paneTab.viewId)
      : false;
    const documentEntries = slotsRegistry.entriesOfSlot('sidebar.right.tab.document');
    const activeDocKey = paneTab
      ? documentEntries.find(e => e.options.key === paneTab.viewId)?.options.key
      : undefined;
    const hasDocActions = activeDocKey
      ? slotsRegistry
          .entriesOfSlot('sidebar.right.tab.document.action')
          .some(e => e.options.key === activeDocKey)
      : false;

    return (
      <div className="magi-dsh-right-pane" data-testid="dsh-right-pane">
        {paneTab && (tabTypes.some(entry => entry.id === paneTab.viewId) || activeDocKey)
          ? (
            <SessionProvider session={mainSessionRef}>
              {hasTabTitleSlot ? (
                <div className="magi-dsh-right-pane-title" data-testid="dsh-right-pane-title">
                  {renderSlot('sidebar.right.pane.tab.title', {}, { entryKey: paneTab.viewId, hookContext: readTabInfo })}
                </div>
              ) : null}
              {tabTypes.some(entry => entry.id === paneTab.viewId)
                ? renderSlot('sidebar.right.pane.tab', {}, { entryKey: paneTab.viewId, hookContext: readTabInfo })
                : null}
              {activeDocKey ? (
                <div className="magi-dsh-right-pane-document" data-testid="dsh-right-pane-document">
                  {hasDocActions ? (
                    <div className="magi-dsh-right-pane-document-actions">
                      {renderSlot('sidebar.right.tab.document.action', {}, { entryKey: activeDocKey })}
                    </div>
                  ) : null}
                  {renderSlot('sidebar.right.tab.document', {}, { entryKey: activeDocKey })}
                </div>
              ) : null}
            </SessionProvider>
          )
          : null}
      </div>
    );
  }

  const sections = slotsRegistry.entriesOfSlot('settings.section');
  const sectionIcons = new Set(
    slotsRegistry
      .entriesOfSlot('settings.section.icon')
      .map(e => e.options.id)
      .filter((id): id is string => Boolean(id)),
  );
  const settingsPluginTabs = slotsRegistry.entriesOfSlot('settings.plugins.tab');
  const pluginManagerTabs = slotsRegistry.entriesOfSlot('plugins.tab');

  const bundleEntries = slotsRegistry
    .entriesOfSlot('plugins.bundle.config')
    .filter(e => !hostView.pluginId || e.options.key === hostView.pluginId);
  const hasBundleConfig = bundleEntries.length > 0;
  const bundleActivationEntries = slotsRegistry
    .entriesOfSlot('plugins.bundle.activation')
    .filter(e => !hostView.pluginId || e.options.key === hostView.pluginId);
  const pluginItemEntries = slotsRegistry.entriesOfSlot('plugins.item');
  const detailBadges = slotsRegistry.entriesOfSlot('plugins.detail.badge');
  const detailActions = slotsRegistry.entriesOfSlot('plugins.detail.actions');
  const detailSections = slotsRegistry.entriesOfSlot('plugins.detail.section');
  const addActions = slotsRegistry.entriesOfSlot('plugins.add.actions');
  const detailSubject = React.useMemo(
    () => ({ kind: 'bundle' as const, packageName: hostView.pluginId }),
    [hostView.pluginId],
  );

  // Deduplicate row.config when bundle.config already renders the package-level settings card.
  const rowEntries = hasBundleConfig
    ? []
    : slotsRegistry
        .entriesOfSlot('plugins.row.config')
        .filter(
          e =>
            typeof e.options.key === 'string' &&
            (!hostView.pluginId ||
              e.options.key === hostView.pluginId ||
              e.options.key.startsWith(`${hostView.pluginId}#`)),
        );

  const activeSection =
    (selectedSectionId && sections.some(s => s.options.id === selectedSectionId)
      ? selectedSectionId
      : sections[0]?.options.id) ?? null;

  const allPluginTabs = [
    ...settingsPluginTabs.map(e => ({
      kind: 'settings.plugins.tab' as const,
      id: e.options.id ?? 'default',
      label: resolveSlotLabel(e.options.label) ?? e.options.id ?? 'Plugin',
    })),
    ...pluginManagerTabs.map(e => ({
      kind: 'plugins.tab' as const,
      id: e.options.id ?? 'default',
      label: resolveSlotLabel(e.options.label) ?? e.options.id ?? 'Plugin',
    })),
  ];
  const activeTabKey =
    (selectedPluginsTabId && allPluginTabs.some(t => `${t.kind}:${t.id}` === selectedPluginsTabId)
      ? selectedPluginsTabId
      : allPluginTabs[0]
        ? `${allPluginTabs[0].kind}:${allPluginTabs[0].id}`
        : null) ?? null;
  const activeTab = allPluginTabs.find(t => `${t.kind}:${t.id}` === activeTabKey) ?? null;

  const insets = hostView.settingsInsets;
  const shellStyle: React.CSSProperties | undefined =
    insets && (insets.left > 0 || insets.right > 0)
      ? { paddingLeft: `${insets.left}px`, paddingRight: `${insets.right}px` }
      : undefined;

  return (
    <div ref={containerRef} className="magi-dsh-settings-shell" style={shellStyle} data-testid="dsh-settings-shell">
      {sections.length > 1 ? (
        <nav className="magi-dsh-section-nav" aria-label="Plugin sections">
          {sections.map(sec => {
            const id = sec.options.id ?? '';
            const active = id === activeSection;
            const label = resolveSlotLabel(sec.options.label) ?? id;
            return (
              <button
                key={id}
                type="button"
                className={active ? 'magi-dsh-section-btn is-active' : 'magi-dsh-section-btn'}
                onClick={() => setSelectedSectionId(id)}
              >
                {sectionIcons.has(id)
                  ? renderSlot('settings.section.icon', { size: 15, active }, { only: id })
                  : null}
                <span>{label}</span>
              </button>
            );
          })}
        </nav>
      ) : sections.length === 1 && activeSection && sectionIcons.has(activeSection) ? (
        <div className="magi-dsh-section-single-header">
          {renderSlot('settings.section.icon', { size: 16, active: true }, { only: activeSection })}
          <span>{resolveSlotLabel(sections[0].options.label) ?? activeSection}</span>
        </div>
      ) : null}

      {activeSection !== null ? (
        <div className="magi-dsh-slot-block">
          {renderSlot('settings.section', { close: () => bridge.closeSettings() }, { only: activeSection })}
        </div>
      ) : null}

      {(detailBadges.length > 0 || detailActions.length > 0) ? (
        <div className="magi-dsh-slot-block magi-dsh-detail-header" data-testid="dsh-plugin-detail-header">
          {detailBadges.length > 0 ? (
            <div className="magi-dsh-detail-badges">
              {renderSlot('plugins.detail.badge', { subject: detailSubject })}
            </div>
          ) : null}
          {detailActions.length > 0 ? (
            <div className="magi-dsh-detail-actions">
              {renderSlot('plugins.detail.actions', { subject: detailSubject })}
            </div>
          ) : null}
        </div>
      ) : null}

      {bundleActivationEntries.map(entry => {
        const key = entry.options.key ?? hostView.pluginId;
        return (
          <div key={`activation:${key}`} className="magi-dsh-slot-block" data-testid="dsh-plugin-bundle-activation">
            {renderSlot('plugins.bundle.activation', { packageName: key }, { entryKey: key })}
          </div>
        );
      })}

      {pluginItemEntries.map(entry => {
        const id = entry.options.id ?? '';
        const form = id ? configForms?.get(id) : undefined;
        return (
          <div key={`item:${id}`} className="magi-dsh-slot-block" data-testid="dsh-plugin-item">
            {renderSlot('plugins.item', { view: 'page', ...(form ? { form } : {}) }, { only: id })}
          </div>
        );
      })}

      {bundleEntries.map(entry => {
        const key = entry.options.key ?? hostView.pluginId;
        const form = configForms?.get(key);
        return (
          <div key={`bundle:${key}`} className="magi-dsh-slot-block">
            {renderSlot('plugins.bundle.config', { view: 'detail', ...(form ? { form } : {}) }, { entryKey: key })}
          </div>
        );
      })}

      {rowEntries.map(entry => {
        const key = entry.options.key!;
        const hashIndex = key.indexOf('#');
        const rowId = hashIndex >= 0 ? key.slice(hashIndex + 1) : key;
        const form = configForms?.get(rowId);
        return (
          <div key={`row:${key}`} className="magi-dsh-slot-block">
            {renderSlot('plugins.row.config', { view: 'detail', ...(form ? { form } : {}) }, { entryKey: key })}
          </div>
        );
      })}

      {detailSections.length > 0 ? (
        <div className="magi-dsh-slot-block" data-testid="dsh-plugin-detail-sections">
          {renderSlot('plugins.detail.section', { subject: detailSubject })}
        </div>
      ) : null}

      {addActions.length > 0 ? (
        <div className="magi-dsh-slot-block" data-testid="dsh-plugin-add-actions">
          {renderSlot('plugins.add.actions', { onDismiss: () => bridge.closeSettings() })}
        </div>
      ) : null}

      {allPluginTabs.length > 1 && activeTabKey ? (
        <div className="magi-dsh-tabs-bar">
          <SegmentedTabs
            size="sm"
            label="Plugin tabs"
            value={activeTabKey}
            onChange={next => setSelectedPluginsTabId(next)}
            items={allPluginTabs.map((item, idx) => ({
              value: `${item.kind}:${item.id}`,
              label: item.label,
              id: `dsh-ptab-${idx}`,
              panelId: `dsh-ptab-panel-${idx}`,
            }))}
          />
        </div>
      ) : null}

      {activeTab ? (
        <div className="magi-dsh-slot-block">
          {activeTab.kind === 'settings.plugins.tab'
            ? renderSlot('settings.plugins.tab', {}, { only: activeTab.id })
            : renderSlot(
                'plugins.tab',
                {
                  install: (spec: unknown) => bridge.installPlugin(spec),
                  update: (pkg: string, ver?: string) => bridge.updatePlugin(pkg, ver),
                  openPackage: (pkg: string) => bridge.openPackage(pkg),
                },
                { only: activeTab.id },
              )}
        </div>
      ) : null}
    </div>
  );
}
