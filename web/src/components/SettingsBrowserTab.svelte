<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getBrowserCapabilities,
    getBrowserResources,
    reclaimBrowserResources,
    resetWebModels,
    updateBrowserSettings,
    type BrowserCapabilitiesSnapshot,
    type BrowserCapabilitySettings,
    type BrowserResourceTabSnapshot,
    type BrowserResourcesSnapshot,
  } from '../web/agent-api';
  import {
    checkForDesktopUpdate,
    desktopUpdaterState,
  } from '../stores/desktop-updater.svelte';
  import { applyWebModelRuntime, markWebModelStoppedByUser } from '../stores/web-model-runtime.svelte';
  import { confirmAction } from '../stores/confirm-dialog.svelte';

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
  let savingSetting = $state<keyof BrowserCapabilitySettings | ''>('');
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
    const confirmed = await confirmAction({
      title: i18n.t('settings.browser.resourcesReclaim'),
      message: i18n.t('settings.browser.resourcesReclaimConfirm', { count: selected.length }),
      confirmLabel: i18n.t('settings.browser.resourcesReclaim'),
      cancelLabel: i18n.t('common.cancel'),
      tone: 'danger',
    });
    if (!confirmed) return;
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
    key: keyof BrowserCapabilitySettings,
    enabled: boolean,
  ): Promise<void> {
    if (!isDesktop || !capabilitySnapshot || savingSetting) return;
    savingSetting = key;
    capabilityError = '';
    try {
      capabilitySnapshot = await updateBrowserSettings({
        inAppBrowserEnabled: capabilitySnapshot.inAppBrowserEnabled,
        browserUseEnabled: capabilitySnapshot.browserUseEnabled,
        devtoolsEnabled: capabilitySnapshot.devtoolsEnabled,
        lanAccessEnabled: capabilitySnapshot.lanAccessEnabled,
        [key]: enabled,
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
        const confirmed = await confirmAction({
          title: i18n.t('settings.browser.clearData'),
          message: i18n.t('settings.browser.clearDataConfirm'),
          confirmLabel: i18n.t('settings.browser.clearData'),
          cancelLabel: i18n.t('common.cancel'),
          tone: 'danger',
        });
        if (!confirmed) return;
        // 通用清理也会清掉 GPT Web 的登录分区：必须先让 daemon 取消在飞回复、释放槽位并隐藏入口，
        // 否则槽位会悬挂在一个已失效的页面上。顺序与设置里 GPT Web 的「清除数据」一致。
        await resetWebModels();
        markWebModelStoppedByUser(true);
        applyWebModelRuntime(null);
        try {
          await desktop.clearBrowserData();
        } catch (error) {
          // GPT Web 此时已经停止：明确告诉用户处于「已停止、数据未清」的中间状态，而不是笼统的失败。
          throw new Error(i18n.t('settings.browser.clearDataPartial', { reason: errorMessage(error) }));
        }
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

  /** Electron / Chromium 是否就绪取决于主进程是否报告了真实版本号，而不是「拿到了桌面信息」。 */
  function browserEngineStatus(version: string | null | undefined): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    return version?.trim()
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

<div class="settings-frame">
    <section class="settings-section" aria-labelledby="browser-capabilities-title">
      <div class="settings-section-header">
        <div>
          <div class="settings-section-title" id="browser-capabilities-title">{i18n.t('settings.browser.capabilitiesTitle')}</div>
          <div class="settings-section-desc">{i18n.t('settings.browser.capabilitiesDescription')}</div>
        </div>
      </div>
      <div class="settings-rows">
        <div class="settings-row">
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.browser.inAppBrowser')}</span>
            <span class="settings-row-desc">{i18n.t('settings.browser.inAppBrowserDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.inAppBrowserEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.inAppBrowser')}
            onchange={(enabled) => void saveCapabilitySetting('inAppBrowserEnabled', enabled)}
          />
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.browser.browserUse')}</span>
            <span class="settings-row-desc">{i18n.t('settings.browser.browserUseDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.browserUseEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.browserUse')}
            onchange={(enabled) => void saveCapabilitySetting('browserUseEnabled', enabled)}
          />
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.browser.devtools')}</span>
            <span class="settings-row-desc">{i18n.t('settings.browser.devtoolsDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.devtoolsEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.devtools')}
            onchange={(enabled) => void saveCapabilitySetting('devtoolsEnabled', enabled)}
          />
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.browser.lanAccess')}</span>
            <span class="settings-row-desc">{i18n.t('settings.browser.lanAccessDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.lanAccessEnabled ?? false}
            disabled={!isDesktop || !capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.lanAccess')}
            onchange={(enabled) => void saveCapabilitySetting('lanAccessEnabled', enabled)}
          />
        </div>
      </div>
      {#if capabilityError}
        <div class="settings-notice settings-notice--error" role="status">
          <Icon name="alert-circle" size={14} />
          <span>{capabilityError}</span>
        </div>
      {/if}
    </section>

    <section class="settings-section browser-resource-section" aria-labelledby="browser-resources-title">
      <div class="settings-section-header">
        <div>
          <div class="settings-section-title" id="browser-resources-title">{i18n.t('settings.browser.resourcesTitle')}</div>
          <div class="settings-section-desc">{i18n.t('settings.browser.resourcesDescription')}</div>
        </div>
      </div>

      <div class="settings-rows" aria-live="polite">
      <div class="settings-row">
        <div class="settings-row-copy">
          <span class="settings-row-title">
            {browserResources
              ? i18n.t('settings.browser.resourcesUsage', {
                  used: browserResources.liveTabs,
                  max: browserResources.maxTabs,
                })
              : i18n.t('settings.browser.resourcesReading')}
          </span>
          <span class="settings-row-desc">
            {browserResources
              ? i18n.t('settings.browser.resourcesReclaimable', { count: browserResources.reclaimableTabs })
              : i18n.t('settings.browser.resourcesReadingDescription')}
          </span>
        </div>
        <button
          type="button"
          class="btn btn--secondary btn--sm"
          onclick={openResourceManager}
          disabled={resourceLoading && !browserResources}
        >
          <Icon name={resourceManagerOpen ? 'chevron-up' : 'chevron-down'} size={12} />
          {i18n.t(resourceManagerOpen ? 'settings.browser.resourcesCollapse' : 'settings.browser.resourcesManage')}
        </button>
      </div>
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
                class="plugin-link-btn"
                onclick={selectAllReclaimableResources}
                disabled={resourceLoading || resourceReclaiming || !browserResources?.reclaimableTabs}
              >{i18n.t('settings.browser.resourcesSelectAll')}</button>
              <button
                type="button"
                class="plugin-link-btn"
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
            <div class="settings-notice settings-notice--error" role="alert">
              <Icon name="alert-circle" size={14} /><span>{resourceError}</span>
            </div>
          {/if}

          <div class="resource-manager-footer">
            <span>{i18n.t('settings.browser.resourcesWarning')}</span>
            <button
              type="button"
              class="btn btn--error btn--sm"
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
        <div class="settings-section-header">
          <div>
            <div class="settings-section-title" id="browser-components-title">{i18n.t('settings.browser.componentsTitle')}</div>
            <div class="settings-section-desc">{i18n.t('settings.browser.componentsDescription')}</div>
          </div>
          <button
            type="button"
            class="btn-icon btn-icon--sm"
            class:refreshing={activeAction === 'refresh-components' || desktopLoading}
            onclick={() => void runDesktopAction('refresh-components')}
            disabled={componentActionDisabled('refresh-components')}
            aria-busy={desktopLoading || activeAction === 'refresh-components'}
            title={i18n.t('settings.browser.refreshComponents')}
            aria-label={i18n.t('settings.browser.refreshComponents')}
          ><Icon name="refresh" size={14} /></button>
        </div>

        <div class="settings-rows" aria-live="polite">
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.desktopHost')}</span><span class="settings-row-desc">{hostComponentStatus()}</span></div>
            <code>{versionText(desktopInfo?.product_version)}</code>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.electron')}</span><span class="settings-row-desc">{browserEngineStatus(desktopInfo?.electron_version)}</span></div>
            <code>{versionText(desktopInfo?.electron_version)}</code>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.chromium')}</span><span class="settings-row-desc">{browserEngineStatus(desktopInfo?.chromium_version)}</span></div>
            <code>{versionText(desktopInfo?.chromium_version)}</code>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.daemon')}</span><span class="settings-row-desc">{componentStatus(desktopInfo?.daemon.status)}</span></div>
            <code>{versionText(desktopInfo?.daemon.version)}</code>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.automationWorker')}</span><span class="settings-row-desc">{componentStatus(desktopInfo?.worker.status)}</span></div>
            <code>{versionText(desktopInfo?.worker.version)}</code>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('settings.browser.component.protocol')}</span><span class="settings-row-desc">{protocolComponentStatus()}</span></div>
            <code>{desktopInfo ? protocolVersion() : i18n.t('settings.browser.versionUnavailable')}</code>
          </div>
        </div>
        {#if desktopInfo?.error}
          <div class="settings-notice settings-notice--error" role="status" aria-live="assertive">
            <Icon name="alert-circle" size={14} />
            <span>{desktopInfo.error.target}: <code>{desktopInfo.error.message}</code></span>
          </div>
        {/if}
      </section>

      <section class="settings-section desktop-actions" aria-labelledby="browser-actions-title">
        <div class="settings-section-header">
          <div>
            <div class="settings-section-title" id="browser-actions-title">{i18n.t('settings.browser.actionsTitle')}</div>
            <div class="settings-section-desc">{i18n.t('settings.browser.actionsDescription')}</div>
          </div>
        </div>

        <div class="settings-rows">
          <div class="settings-row">
          <div class="settings-row-copy">
              <span class="settings-row-title">{i18n.t('settings.browser.restartAutomation')}</span>
              <span class="settings-row-desc">{i18n.t('settings.browser.restartAutomationDescription')}</span>
            </div>
            <button
              type="button"
              class="btn btn--secondary btn--sm"
              onclick={() => void runDesktopAction('restart-automation')}
              disabled={componentActionDisabled('restart-automation')}
              aria-busy={activeAction === 'restart-automation'}
            >{actionLabel('restart-automation')}</button>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy">
              <span class="settings-row-title">{i18n.t('settings.browser.clearData')}</span>
              <span class="settings-row-desc">{i18n.t('settings.browser.clearDataDescription')}</span>
            </div>
            <button
              type="button"
              class="btn btn--error btn--sm"
              onclick={() => void runDesktopAction('clear-data')}
              disabled={componentActionDisabled('clear-data')}
              aria-busy={activeAction === 'clear-data'}
            >{actionLabel('clear-data')}</button>
          </div>
          <div class="settings-row">
          <div class="settings-row-copy">
              <span class="settings-row-title">{i18n.t('settings.browser.checkDesktopUpdates')}</span>
              <span class="settings-row-desc">{i18n.t('settings.browser.checkDesktopUpdatesDescription')}</span>
              <span class="settings-row-desc">{updateStatusText()}</span>
            </div>
            <button
              type="button"
              class="btn btn--secondary btn--sm"
              onclick={() => void runDesktopAction('check-updates')}
              disabled={componentActionDisabled('check-updates')}
              aria-busy={activeAction === 'check-updates'}
            >{actionLabel('check-updates')}</button>
          </div>
        </div>

        {#if desktopError}
          <div class="settings-notice settings-notice--error" role="status" aria-live="assertive">
            <Icon name="alert-circle" size={14} />
            <span>{i18n.t('settings.browser.actionFailed')}: <code>{desktopError}</code></span>
          </div>
        {:else if actionNotice}
          <div class="settings-notice settings-notice--success" role="status" aria-live="polite">
            <Icon name="check-circle" size={14} />
            <span>{actionNotice}</span>
          </div>
        {/if}
      </section>
    {:else}
      <section class="settings-section desktop-unavailable" aria-labelledby="browser-desktop-unavailable-title">
        <div>
          <div class="settings-section-title" id="browser-desktop-unavailable-title">{i18n.t('settings.browser.webUnavailableTitle')}</div>
          <div class="settings-section-desc">{i18n.t('settings.browser.webUnavailableDescription')}</div>
        </div>
      </section>
    {/if}

  </div>

<style>
  /* 视觉来自 settings.css 的共享类与全局 .btn；这里只保留“页面回收管理器”特有的布局。 */
  .resource-manager { padding-top: 12px; }
  .resource-manager-toolbar,
  .resource-manager-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .resource-manager-toolbar { color: var(--foreground-muted); font-size: var(--text-xs); }
  .resource-manager-toolbar > div { display: inline-flex; gap: 10px; }
  .plugin-link-btn {
    padding: 0;
    border: 0;
    color: var(--primary);
    background: transparent;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .plugin-link-btn:disabled { cursor: default; opacity: 0.42; }
  .resource-list {
    margin-top: 8px;
    border-top: var(--magi-hairline) solid var(--border-subtle);
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
  }
  .resource-row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 52px;
    padding: 7px 2px;
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
  }
  .resource-row:last-child { border-bottom: 0; }
  .resource-row input { flex: 0 0 auto; width: 14px; height: 14px; accent-color: var(--primary); }
  .resource-row-copy { min-width: 0; flex: 1; }
  .resource-row-copy strong,
  .resource-row-copy span,
  .resource-row-copy small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .resource-row-copy strong { color: var(--foreground); font-size: var(--text-xs); font-weight: var(--font-semibold); }
  .resource-row-copy .resource-page { margin-top: 2px; color: var(--foreground-muted); font-size: var(--text-xs); }
  .resource-row-copy small {
    margin-top: 2px;
    color: var(--foreground-muted);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: var(--text-xs);
  }
  .resource-row-status { flex: 0 0 auto; max-width: 104px; color: var(--foreground-muted); font-size: var(--text-xs); text-align: right; }
  .resource-row-status--ready { color: var(--primary); }
  .resource-manager-footer { align-items: flex-start; padding-top: 10px; }
  .resource-manager-footer > span { max-width: 64%; color: var(--foreground-muted); font-size: var(--text-xs); line-height: 1.45; }
  .resource-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-height: 74px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
  .resource-empty :global(svg),
  .refreshing :global(svg) { animation: browser-components-refresh-spin 0.9s linear infinite; }
  @keyframes browser-components-refresh-spin { to { transform: rotate(360deg); } }
  @media (max-width: 560px) {
    .resource-manager-footer { flex-direction: column; }
    .resource-manager-footer > span { max-width: none; }
  }
</style>
