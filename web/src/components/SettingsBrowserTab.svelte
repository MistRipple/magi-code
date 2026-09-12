<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getBrowserCapabilities,
    getBrowserResources,
    reclaimBrowserResources,
    updateBrowserSettings,
    type BrowserCapabilitiesSnapshot,
    type BrowserResourceTabSnapshot,
    type BrowserResourcesSnapshot,
  } from '../web/agent-api';
  import {
    checkForDesktopUpdate,
    desktopUpdaterState,
  } from '../stores/desktop-updater.svelte';

  type DesktopAction = 'refresh-components' | 'restart-automation' | 'clear-data' | 'check-updates';

  let capabilitySnapshot = $state<BrowserCapabilitiesSnapshot | null>(null);
  let desktopInfo = $state<MagiDesktopBrowserComponentSnapshot | null>(null);
  let isDesktop = $state(false);
  let capabilityLoading = $state(false);
  let desktopLoading = $state(false);
  let capabilityError = $state('');
  let desktopError = $state('');
  let resourceError = $state('');
  let actionNotice = $state('');
  let activeAction = $state<DesktopAction | ''>('');
  let savingSetting = $state<'inAppBrowserEnabled' | 'browserUseEnabled' | ''>('');
  let browserResources = $state<BrowserResourcesSnapshot | null>(null);
  let resourceManagerOpen = $state(false);
  let resourceLoading = $state(false);
  let resourceReclaiming = $state(false);
  let selectedResourceTabIds = $state<string[]>([]);
  let actionNoticeTimer: ReturnType<typeof setTimeout> | null = null;

  function clearActionFeedback(): void {
    if (actionNoticeTimer !== null) {
      clearTimeout(actionNoticeTimer);
      actionNoticeTimer = null;
    }
    actionNotice = '';
    desktopError = '';
  }

  function showActionNotice(message: string): void {
    clearActionFeedback();
    actionNotice = message;
    actionNoticeTimer = setTimeout(() => {
      actionNotice = '';
      actionNoticeTimer = null;
    }, 3500);
  }

  function errorMessage(error: unknown): string {
    if (error instanceof Error && error.message.trim()) return error.message.trim();
    return i18n.t('settings.browser.actionFailed');
  }

  function versionText(value: string | null | undefined): string {
    return value?.trim() || i18n.t('settings.browser.versionUnavailable');
  }

  async function loadCapabilities(): Promise<void> {
    if (capabilityLoading) return;
    capabilityLoading = true;
    capabilityError = '';
    try {
      capabilitySnapshot = await getBrowserCapabilities();
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取浏览器能力设置失败:', error);
      capabilityError = i18n.t('settings.browser.loadCapabilitiesFailed');
    } finally {
      capabilityLoading = false;
    }
  }

  function defaultResourceSelection(snapshot: BrowserResourcesSnapshot): string[] {
    return snapshot.tabs
      .filter((tab) => tab.canReclaim && !tab.isCurrentSession)
      .map((tab) => tab.tabId);
  }

  async function loadBrowserResources(options: { selectDefaults?: boolean } = {}): Promise<void> {
    if (resourceLoading) return;
    resourceLoading = true;
    resourceError = '';
    try {
      const snapshot = await getBrowserResources();
      browserResources = snapshot;
      const availableIds = new Set(snapshot.tabs.filter((tab) => tab.canReclaim).map((tab) => tab.tabId));
      selectedResourceTabIds = options.selectDefaults === true && isDesktop
        ? defaultResourceSelection(snapshot)
        : selectedResourceTabIds.filter((tabId) => availableIds.has(tabId));
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取浏览器页面资源失败:', error);
      resourceError = i18n.t('settings.browser.resourcesLoadFailed');
    } finally {
      resourceLoading = false;
    }
  }

  function openResourceManager(): void {
    resourceManagerOpen = !resourceManagerOpen;
    if (resourceManagerOpen) {
      void loadBrowserResources({ selectDefaults: true });
    }
  }

  function toggleResourceTab(tabId: string, selected: boolean): void {
    if (!isDesktop) return;
    if (selected) {
      if (!selectedResourceTabIds.includes(tabId)) {
        selectedResourceTabIds = [...selectedResourceTabIds, tabId];
      }
      return;
    }
    selectedResourceTabIds = selectedResourceTabIds.filter((candidate) => candidate !== tabId);
  }

  function selectAllReclaimableResources(): void {
    if (!isDesktop) return;
    selectedResourceTabIds = browserResources?.tabs
      .filter((tab) => tab.canReclaim)
      .map((tab) => tab.tabId) ?? [];
  }

  async function reclaimSelectedResources(): Promise<void> {
    const selected = selectedResourceTabIds;
    if (!isDesktop || resourceReclaiming || selected.length === 0) return;
    if (!window.confirm(i18n.t('settings.browser.resourcesReclaimConfirm', { count: selected.length }))) return;
    resourceReclaiming = true;
    resourceError = '';
    try {
      const result = await reclaimBrowserResources(selected);
      browserResources = result.resources;
      selectedResourceTabIds = defaultResourceSelection(result.resources);
      const message = result.skipped.length > 0
        ? i18n.t('settings.browser.resourcesReclaimedWithSkipped', {
            count: result.reclaimedCount,
            skipped: result.skipped.length,
          })
        : i18n.t('settings.browser.resourcesReclaimed', { count: result.reclaimedCount });
      showActionNotice(message);
    } catch (error) {
      console.warn('[SettingsBrowserTab] 回收浏览器页面资源失败:', error);
      resourceError = errorMessage(error);
    } finally {
      resourceReclaiming = false;
    }
  }

  function resourcePageLabel(tab: BrowserResourceTabSnapshot): string {
    return tab.title.trim() || tab.url.trim() || i18n.t('settings.browser.resourcesUntitled');
  }

  function resourceStatusLabel(tab: BrowserResourceTabSnapshot): string {
    if (tab.isCurrentTab) return i18n.t('settings.browser.resourcesCurrentTab');
    if (tab.sessionRunning) return i18n.t('settings.browser.resourcesSessionRunning');
    if (tab.lifecycle === 'suspended') return i18n.t('settings.browser.resourcesSuspended');
    if (tab.lifecycle === 'crashed') return i18n.t('settings.browser.resourcesCrashed');
    return i18n.t('settings.browser.resourcesActive');
  }

  async function fetchDesktopInfo(): Promise<MagiDesktopBrowserComponentSnapshot> {
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop) throw new Error('desktop_runtime_unavailable');
    desktopLoading = true;
    desktopError = '';
    try {
      const next = await desktop.getBrowserComponentInfo();
      desktopInfo = next;
      return next;
    } finally {
      desktopLoading = false;
    }
  }

  async function loadDesktopInfo(): Promise<void> {
    if (!isDesktop || !window.magiDesktop || desktopLoading) return;
    try {
      await fetchDesktopInfo();
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取桌面浏览器组件状态失败:', error);
      desktopInfo = null;
      desktopError = errorMessage(error);
    }
  }

  async function saveCapabilitySetting(
    key: 'inAppBrowserEnabled' | 'browserUseEnabled',
    enabled: boolean,
  ): Promise<void> {
    if (!isDesktop || !capabilitySnapshot || savingSetting) return;
    savingSetting = key;
    capabilityError = '';
    try {
      capabilitySnapshot = await updateBrowserSettings({
        inAppBrowserEnabled: key === 'inAppBrowserEnabled'
          ? enabled
          : capabilitySnapshot.inAppBrowserEnabled,
        browserUseEnabled: key === 'browserUseEnabled'
          ? enabled
          : capabilitySnapshot.browserUseEnabled,
      });
      window.dispatchEvent(new CustomEvent('magi:browserCapabilitiesChanged', {
        detail: capabilitySnapshot,
      }));
    } catch (error) {
      console.warn('[SettingsBrowserTab] 保存浏览器能力设置失败:', error);
      capabilityError = i18n.t('settings.browser.saveFailed');
    } finally {
      savingSetting = '';
    }
  }

  async function runDesktopAction(action: DesktopAction): Promise<void> {
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop || activeAction) return;
    activeAction = action;
    clearActionFeedback();
    try {
      if (action === 'refresh-components') {
        await fetchDesktopInfo();
        showActionNotice(i18n.t('settings.browser.refreshSucceeded'));
      } else if (action === 'restart-automation') {
        desktopInfo = await desktop.restartBrowserAutomation();
        showActionNotice(i18n.t('settings.browser.restartAutomationSucceeded'));
      } else if (action === 'clear-data') {
        if (!window.confirm(i18n.t('settings.browser.clearDataConfirm'))) return;
        await desktop.clearBrowserData();
        showActionNotice(i18n.t('settings.browser.clearDataSucceeded'));
      } else {
        const result = await checkForDesktopUpdate('manual');
        if (result === 'error') {
          throw new Error(desktopUpdaterState.error || i18n.t('settings.browser.actionFailed'));
        }
        if (result === 'available') {
          showActionNotice(i18n.t('settings.browser.desktopUpdateAvailable', {
            version: desktopUpdaterState.update?.version || '',
          }));
        } else if (result === 'latest') {
          showActionNotice(i18n.t('settings.browser.desktopUpToDate'));
        } else if (result === 'ignored') {
          showActionNotice(updateStatusText());
        }
      }
    } catch (error) {
      console.warn(`[SettingsBrowserTab] 桌面浏览器操作失败: ${action}`, error);
      desktopError = errorMessage(error);
    } finally {
      activeAction = '';
    }
  }

  function componentStatus(
    status: MagiDesktopBrowserComponentStatus | 'protocol-incompatible' | undefined,
  ): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    return i18n.t(`settings.browser.status.${status ?? 'unavailable'}`);
  }

  function browserRuntimeReady(): boolean {
    return desktopInfo?.runtime.ready === true
      && capabilitySnapshot?.hostStatus === 'ready'
      && capabilitySnapshot.hostProtocolCompatible;
  }

  function hostComponentStatus(): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    if (!desktopInfo) return i18n.t('settings.browser.status.unavailable');
    if (desktopInfo.runtime.status !== 'ready') return componentStatus(desktopInfo.runtime.status);
    if (!capabilitySnapshot) return i18n.t('settings.browser.status.unavailable');
    if (capabilitySnapshot.lastErrorCode === 'browser_protocol_incompatible') {
      return i18n.t('settings.browser.status.protocol-incompatible');
    }
    if (capabilitySnapshot.hostStatus !== 'ready') {
      return componentStatus(capabilityHostStatus());
    }
    if (!capabilitySnapshot.hostProtocolCompatible) {
      return i18n.t('settings.browser.status.protocol-incompatible');
    }
    return browserRuntimeReady()
      ? i18n.t('settings.browser.status.ready')
      : componentStatus(desktopInfo.runtime.status);
  }

  function browserEngineStatus(): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    return desktopInfo
      ? i18n.t('settings.browser.status.ready')
      : i18n.t('settings.browser.status.unavailable');
  }

  function capabilityHostStatus(): MagiDesktopBrowserComponentStatus {
    switch (capabilitySnapshot?.hostStatus) {
      case 'starting': return 'starting';
      case 'reconnecting': return 'restarting';
      case 'failed': return 'failed';
      case 'stopped': return 'stopped';
      case 'ready': return 'ready';
      default: return 'failed';
    }
  }

  function protocolComponentStatus(): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    if (!desktopInfo) return i18n.t('settings.browser.status.unavailable');
    if (desktopInfo.protocol.status === 'incompatible') {
      return i18n.t('settings.browser.status.protocol-incompatible');
    }
    return componentStatus(desktopInfo.protocol.status);
  }

  function componentActionDisabled(action: DesktopAction): boolean {
    if (!isDesktop || !window.magiDesktop || desktopLoading || Boolean(activeAction)) return true;
    if (
      action === 'check-updates'
      && ['checking', 'downloading', 'ready', 'installing'].includes(desktopUpdaterState.phase)
    ) {
      return true;
    }
    return false;
  }

  function protocolVersion(): string {
    const version = desktopInfo?.protocol.version;
    return version ? `${version.major}.${version.minor}` : '-';
  }

  function updateStatusText(): string {
    const status = desktopUpdaterState.phase;
    if (status === 'idle') {
      return desktopUpdaterState.lastCheckedAt > 0
        ? i18n.t('settings.browser.updateStatus.idle')
        : i18n.t('settings.browser.updateStatus.notChecked');
    }
    if (status === 'latest') return i18n.t('settings.browser.updateStatus.idle');
    if (status === 'ready') return i18n.t('settings.browser.updateStatus.downloaded');
    if (status === 'error') return i18n.t('settings.browser.updateStatus.failed');
    if (status === 'installing') return i18n.t('settings.browser.updateStatus.downloaded');
    return i18n.t(`settings.browser.updateStatus.${status}`);
  }

  function actionLabel(action: DesktopAction): string {
    if (activeAction !== action) {
      if (action === 'refresh-components') return i18n.t('settings.browser.refreshComponents');
      if (action === 'restart-automation') return i18n.t('settings.browser.restartAutomation');
      if (action === 'clear-data') return i18n.t('settings.browser.clearData');
      return i18n.t('settings.browser.checkDesktopUpdates');
    }
    if (action === 'refresh-components') return i18n.t('settings.browser.refreshingComponents');
    if (action === 'restart-automation') return i18n.t('settings.browser.restartingAutomation');
    if (action === 'clear-data') return i18n.t('settings.browser.clearingData');
    return i18n.t('settings.browser.checkingDesktopUpdates');
  }

  onMount(() => {
    const desktop = window.magiDesktop;
    isDesktop = desktop?.runtime === 'electron';
    void loadCapabilities();
    void loadBrowserResources({ selectDefaults: true });

    let unsubscribeComponent: (() => void) | null = null;
    if (isDesktop && desktop) {
      void loadDesktopInfo();
      unsubscribeComponent = desktop.onBrowserComponent((next) => {
        desktopInfo = next;
      });
    }

    return () => {
      unsubscribeComponent?.();
      if (actionNoticeTimer !== null) clearTimeout(actionNoticeTimer);
    };
  });
</script>

<div class="settings-tab-inner browser-settings-tab">
  <div class="browser-settings-content">
    <section class="settings-section" aria-labelledby="browser-capabilities-title">
      <div class="section-heading">
        <div>
          <h3 id="browser-capabilities-title">{i18n.t('settings.browser.capabilitiesTitle')}</h3>
          <p>{i18n.t('settings.browser.capabilitiesDescription')}</p>
        </div>
      </div>
      <div class="capability-list">
        <div class="capability-row">
          <div>
            <strong>{i18n.t('settings.browser.inAppBrowser')}</strong>
            <span>{i18n.t('settings.browser.inAppBrowserDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.inAppBrowserEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.inAppBrowser')}
            onchange={(enabled) => void saveCapabilitySetting('inAppBrowserEnabled', enabled)}
          />
        </div>
        <div class="capability-row">
          <div>
            <strong>{i18n.t('settings.browser.browserUse')}</strong>
            <span>{i18n.t('settings.browser.browserUseDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.browserUseEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.browserUse')}
            onchange={(enabled) => void saveCapabilitySetting('browserUseEnabled', enabled)}
          />
        </div>
      </div>
      {#if capabilityError}
        <div class="status-message status-message--error" role="status">
          <Icon name="alert-circle" size={14} />
          <span>{capabilityError}</span>
        </div>
      {/if}
    </section>

    <section class="settings-section browser-resource-section" aria-labelledby="browser-resources-title">
      <div class="section-heading">
        <div class="section-icon" aria-hidden="true"><Icon name="database" size={17} /></div>
        <div>
          <h3 id="browser-resources-title">{i18n.t('settings.browser.resourcesTitle')}</h3>
          <p>{i18n.t('settings.browser.resourcesDescription')}</p>
        </div>
      </div>

      <div class="resource-summary" aria-live="polite">
        <div class="resource-summary-copy">
          <strong>
            {browserResources
              ? i18n.t('settings.browser.resourcesUsage', {
                  used: browserResources.liveTabs,
                  max: browserResources.maxTabs,
                })
              : i18n.t('settings.browser.resourcesReading')}
          </strong>
          <span>
            {browserResources
              ? i18n.t('settings.browser.resourcesReclaimable', { count: browserResources.reclaimableTabs })
              : i18n.t('settings.browser.resourcesReadingDescription')}
          </span>
        </div>
        <button
          type="button"
          class="resource-manage-button"
          onclick={openResourceManager}
          disabled={resourceLoading && !browserResources}
        >
          <Icon name={resourceManagerOpen ? 'chevron-up' : 'chevron-down'} size={12} />
          {i18n.t(resourceManagerOpen ? 'settings.browser.resourcesCollapse' : 'settings.browser.resourcesManage')}
        </button>
      </div>

      {#if resourceManagerOpen}
        <div class="resource-manager" aria-busy={resourceLoading || resourceReclaiming}>
          <div class="resource-manager-toolbar">
            <span>
              {i18n.t('settings.browser.resourcesSelected', { count: selectedResourceTabIds.length })}
            </span>
            <div>
              <button
                type="button"
                class="resource-text-button"
                onclick={selectAllReclaimableResources}
                disabled={resourceLoading || resourceReclaiming || !browserResources?.reclaimableTabs}
              >{i18n.t('settings.browser.resourcesSelectAll')}</button>
              <button
                type="button"
                class="resource-text-button"
                onclick={() => { selectedResourceTabIds = []; }}
                disabled={resourceLoading || resourceReclaiming || selectedResourceTabIds.length === 0}
              >{i18n.t('settings.browser.resourcesClearSelection')}</button>
            </div>
          </div>

          {#if resourceLoading && !browserResources}
            <div class="resource-empty" role="status"><Icon name="loader" size={14} />{i18n.t('settings.browser.resourcesReading')}</div>
          {:else if browserResources?.tabs.length === 0}
            <div class="resource-empty">{i18n.t('settings.browser.resourcesEmpty')}</div>
          {:else if browserResources}
            <div class="resource-list">
              {#each browserResources.tabs as tab (tab.tabId)}
                <label class="resource-row" class:resource-row--blocked={!tab.canReclaim}>
                  <input
                    type="checkbox"
                    checked={selectedResourceTabIds.includes(tab.tabId)}
                    disabled={!isDesktop || !tab.canReclaim || resourceLoading || resourceReclaiming}
                    onchange={(event) => toggleResourceTab(tab.tabId, (event.currentTarget as HTMLInputElement).checked)}
                  />
                  <span class="resource-row-copy">
                    <strong>{tab.sessionTitle || tab.sessionId}</strong>
                    <span class="resource-page">{resourcePageLabel(tab)}</span>
                    <small>{tab.url}</small>
                  </span>
                  <span class="resource-row-status" class:resource-row-status--ready={tab.canReclaim}>
                    {resourceStatusLabel(tab)}
                  </span>
                </label>
              {/each}
            </div>
          {/if}

          {#if resourceError}
            <div class="status-message status-message--error" role="alert">
              <Icon name="alert-circle" size={14} /><span>{resourceError}</span>
            </div>
          {/if}

          <div class="resource-manager-footer">
            <span>{i18n.t('settings.browser.resourcesWarning')}</span>
            <button
              type="button"
              class="danger resource-reclaim-button"
              onclick={() => void reclaimSelectedResources()}
              disabled={!isDesktop || resourceLoading || resourceReclaiming || selectedResourceTabIds.length === 0}
              aria-busy={resourceReclaiming}
            >
              <Icon name="trash" size={13} />
              {resourceReclaiming
                ? i18n.t('settings.browser.resourcesReclaiming')
                : i18n.t('settings.browser.resourcesReclaim')}
            </button>
          </div>
        </div>
      {/if}
    </section>

    {#if isDesktop}
      <section class="settings-section desktop-components" aria-labelledby="browser-components-title">
        <div class="section-heading">
          <div class="section-icon" aria-hidden="true"><Icon name="globe" size={18} /></div>
          <div>
            <h3 id="browser-components-title">{i18n.t('settings.browser.componentsTitle')}</h3>
            <p>{i18n.t('settings.browser.componentsDescription')}</p>
          </div>
          <button
            type="button"
            class="icon-action"
            class:icon-action--loading={activeAction === 'refresh-components' || desktopLoading}
            onclick={() => void runDesktopAction('refresh-components')}
            disabled={componentActionDisabled('refresh-components')}
            aria-busy={desktopLoading || activeAction === 'refresh-components'}
            title={i18n.t('settings.browser.refreshComponents')}
            aria-label={i18n.t('settings.browser.refreshComponents')}
          ><Icon name="refresh" size={14} /></button>
        </div>

        <div class="component-list" aria-live="polite">
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.desktopHost')}</strong><span>{hostComponentStatus()}</span></div>
            <code>{versionText(desktopInfo?.product_version)}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.electron')}</strong><span>{browserEngineStatus()}</span></div>
            <code>{versionText(desktopInfo?.electron_version)}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.chromium')}</strong><span>{browserEngineStatus()}</span></div>
            <code>{versionText(desktopInfo?.chromium_version)}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.daemon')}</strong><span>{componentStatus(desktopInfo?.daemon.status)}</span></div>
            <code>{versionText(desktopInfo?.daemon.version)}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.automationWorker')}</strong><span>{componentStatus(desktopInfo?.worker.status)}</span></div>
            <code>{versionText(desktopInfo?.worker.version)}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.protocol')}</strong><span>{protocolComponentStatus()}</span></div>
            <code>{desktopInfo ? protocolVersion() : i18n.t('settings.browser.versionUnavailable')}</code>
          </div>
        </div>
        {#if desktopInfo?.error}
          <div class="status-message status-message--error" role="status" aria-live="assertive">
            <Icon name="alert-circle" size={14} />
            <span>{desktopInfo.error.target}: <code>{desktopInfo.error.message}</code></span>
          </div>
        {/if}
      </section>

      <section class="settings-section desktop-actions" aria-labelledby="browser-actions-title">
        <div class="section-heading">
          <div>
            <h3 id="browser-actions-title">{i18n.t('settings.browser.actionsTitle')}</h3>
            <p>{i18n.t('settings.browser.actionsDescription')}</p>
          </div>
        </div>

        <div class="action-list">
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.restartAutomation')}</strong>
              <span>{i18n.t('settings.browser.restartAutomationDescription')}</span>
            </div>
            <button
              type="button"
              onclick={() => void runDesktopAction('restart-automation')}
              disabled={componentActionDisabled('restart-automation')}
              aria-busy={activeAction === 'restart-automation'}
            >{actionLabel('restart-automation')}</button>
          </div>
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.clearData')}</strong>
              <span>{i18n.t('settings.browser.clearDataDescription')}</span>
            </div>
            <button
              type="button"
              class="danger"
              onclick={() => void runDesktopAction('clear-data')}
              disabled={componentActionDisabled('clear-data')}
              aria-busy={activeAction === 'clear-data'}
            >{actionLabel('clear-data')}</button>
          </div>
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.checkDesktopUpdates')}</strong>
              <span>{i18n.t('settings.browser.checkDesktopUpdatesDescription')}</span>
              <small>{updateStatusText()}</small>
            </div>
            <button
              type="button"
              onclick={() => void runDesktopAction('check-updates')}
              disabled={componentActionDisabled('check-updates')}
              aria-busy={activeAction === 'check-updates'}
            >{actionLabel('check-updates')}</button>
          </div>
        </div>

        {#if desktopError}
          <div class="status-message status-message--error" role="status" aria-live="assertive">
            <Icon name="alert-circle" size={14} />
            <span>{i18n.t('settings.browser.actionFailed')}: <code>{desktopError}</code></span>
          </div>
        {:else if actionNotice}
          <div class="status-message status-message--success" role="status" aria-live="polite">
            <Icon name="check-circle" size={14} />
            <span>{actionNotice}</span>
          </div>
        {/if}
      </section>
    {:else}
      <section class="settings-section desktop-unavailable" aria-labelledby="browser-desktop-unavailable-title">
        <div class="section-icon" aria-hidden="true"><Icon name="globe" size={18} /></div>
        <div>
          <h3 id="browser-desktop-unavailable-title">{i18n.t('settings.browser.webUnavailableTitle')}</h3>
          <p>{i18n.t('settings.browser.webUnavailableDescription')}</p>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .browser-settings-tab {
    padding: 28px clamp(24px, 5vw, 64px) 42px;
    box-sizing: border-box;
  }

  .browser-settings-content {
    width: min(100%, 720px);
    margin: 0 auto;
  }

  .settings-section {
    padding: 0 2px 26px;
  }

  .settings-section + .settings-section {
    padding-top: 26px;
    border-top: 1px solid var(--ind-border-separator);
  }

  .section-heading,
  .capability-row,
  .component-row,
  .action-row,
  .desktop-unavailable {
    display: flex;
    align-items: center;
  }

  .section-heading {
    gap: 12px;
    margin-bottom: 12px;
  }

  .section-heading > div:not(.section-icon),
  .capability-row > div,
  .component-row > div,
  .action-row > div,
  .desktop-unavailable > div:last-child {
    min-width: 0;
    flex: 1;
  }

  h3 {
    margin: 0;
    color: var(--ind-foreground);
    font-size: 14px;
    font-weight: 650;
    letter-spacing: 0;
  }

  p {
    margin: 5px 0 0;
    color: var(--ind-foreground-secondary);
    font-size: 12px;
    line-height: 1.55;
  }

  .capability-list,
  .component-list,
  .action-list {
    border-top: 1px solid var(--ind-border-separator);
  }

  .capability-row,
  .component-row,
  .action-row {
    gap: 20px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .capability-row { min-height: 58px; }
  .component-row { min-height: 48px; }
  .action-row { min-height: 66px; }

  .capability-row strong,
  .capability-row span,
  .component-row strong,
  .component-row span,
  .action-row strong,
  .action-row span,
  .action-row small {
    display: block;
  }

  .capability-row strong,
  .component-row strong,
  .action-row strong {
    color: var(--ind-foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .capability-row span,
  .component-row span,
  .action-row span,
  .action-row small {
    margin-top: 4px;
    color: var(--ind-foreground-secondary);
    font-size: 11px;
    line-height: 1.45;
  }

  .action-row small {
    color: var(--ind-foreground-muted);
  }

  .component-row code {
    max-width: 50%;
    overflow-wrap: anywhere;
    color: var(--ind-foreground);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 12px;
    text-align: right;
  }

  .section-icon {
    width: 34px;
    height: 34px;
    display: grid;
    flex: 0 0 auto;
    place-items: center;
    border: 1px solid var(--ind-border-control);
    border-radius: 8px;
    color: var(--ind-tab-accent);
    background: var(--ind-bg-control);
  }

  .resource-summary {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 0;
    border-top: 1px solid var(--ind-border-separator);
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .resource-summary-copy {
    min-width: 0;
    flex: 1;
  }

  .resource-summary-copy strong,
  .resource-summary-copy span {
    display: block;
  }

  .resource-summary-copy strong {
    color: var(--ind-foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .resource-summary-copy span {
    margin-top: 4px;
    color: var(--ind-foreground-secondary);
    font-size: 11px;
    line-height: 1.45;
  }

  .resource-manage-button,
  .resource-text-button,
  .resource-reclaim-button {
    cursor: pointer;
  }

  .resource-manage-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
    min-height: 29px;
    padding: 0 10px;
    border: 1px solid var(--ind-border-control);
    border-radius: 6px;
    color: var(--ind-foreground);
    background: var(--ind-bg-control);
    font-size: 12px;
  }

  .resource-manager {
    padding-top: 12px;
  }

  .resource-manager-toolbar,
  .resource-manager-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .resource-manager-toolbar {
    color: var(--ind-foreground-muted);
    font-size: 11px;
  }

  .resource-manager-toolbar > div {
    display: inline-flex;
    gap: 10px;
  }

  .resource-text-button {
    padding: 0;
    border: 0;
    color: var(--ind-tab-accent);
    background: transparent;
    font-size: 11px;
  }

  .resource-text-button:disabled {
    cursor: default;
    opacity: 0.42;
  }

  .resource-list {
    max-height: 280px;
    margin-top: 8px;
    overflow: auto;
    border-top: 1px solid var(--ind-border-separator);
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .resource-row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 52px;
    padding: 7px 2px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .resource-row:last-child { border-bottom: 0; }

  .resource-row input {
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    accent-color: var(--ind-tab-accent);
  }

  .resource-row-copy {
    min-width: 0;
    flex: 1;
  }

  .resource-row-copy strong,
  .resource-row-copy span,
  .resource-row-copy small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .resource-row-copy strong {
    color: var(--ind-foreground);
    font-size: 11px;
    font-weight: 600;
  }

  .resource-row-copy .resource-page {
    margin-top: 2px;
    color: var(--ind-foreground-secondary);
    font-size: 11px;
  }

  .resource-row-copy small {
    margin-top: 2px;
    color: var(--ind-foreground-muted);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 10px;
  }

  .resource-row-status {
    flex: 0 0 auto;
    max-width: 104px;
    color: var(--ind-foreground-muted);
    font-size: 10px;
    text-align: right;
  }

  .resource-row-status--ready {
    color: var(--ind-tab-accent);
  }

  .resource-manager-footer {
    align-items: flex-start;
    padding-top: 10px;
  }

  .resource-manager-footer > span {
    max-width: 64%;
    color: var(--ind-foreground-muted);
    font-size: 10px;
    line-height: 1.45;
  }

  .resource-reclaim-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
    min-height: 29px;
    padding: 0 10px;
    border: 1px solid color-mix(in srgb, var(--danger, #c53f4f) 42%, var(--ind-border-control));
    border-radius: 6px;
    color: var(--danger, #c53f4f);
    background: transparent;
    font-size: 11px;
  }

  .resource-reclaim-button:disabled {
    cursor: default;
    opacity: 0.42;
  }

  .resource-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-height: 74px;
    color: var(--ind-foreground-muted);
    font-size: 11px;
  }

  .resource-empty :global(svg) {
    animation: browser-components-refresh-spin 0.9s linear infinite;
  }

  .icon-action,
  .action-row button {
    border: 1px solid var(--ind-border-control);
    border-radius: 6px;
    color: var(--ind-foreground);
    background: var(--ind-bg-control);
    cursor: pointer;
  }

  .icon-action {
    width: 28px;
    height: 28px;
    padding: 0;
    display: inline-grid;
    place-items: center;
  }

  .action-row button {
    min-width: 82px;
    min-height: 29px;
    padding: 0 10px;
    font-size: 12px;
  }

  .action-row button.danger:not(:disabled) {
    color: var(--danger, #c53f4f);
  }

  button:disabled {
    cursor: default;
    opacity: 0.42;
  }

  .icon-action--loading :global(svg) {
    animation: browser-components-refresh-spin 0.9s linear infinite;
  }

  @keyframes browser-components-refresh-spin {
    to { transform: rotate(360deg); }
  }

  .status-message {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    font-size: 12px;
  }

  .status-message code {
    overflow-wrap: anywhere;
    font-size: 11px;
  }

  .status-message--error { color: var(--danger, #c53f4f); }
  .status-message--success { color: var(--success, #2f9e63); }

  .desktop-unavailable {
    gap: 12px;
  }

  @media (max-width: 560px) {
    .browser-settings-tab { padding-inline: 18px; }
    .section-heading { align-items: flex-start; }
    .component-row code { max-width: 42%; }
    .action-row { align-items: flex-start; padding: 12px 0; }
    .resource-summary { align-items: flex-start; }
    .resource-summary { flex-direction: column; }
    .resource-manage-button { align-self: flex-start; }
    .resource-manager-footer { flex-direction: column; }
    .resource-manager-footer > span { max-width: none; }
  }
</style>
