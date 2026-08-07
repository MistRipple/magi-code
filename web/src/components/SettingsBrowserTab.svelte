<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    AgentApiError,
    BROWSER_AUTHORITY_CHANGED_EVENT,
    getBrowserCapabilities,
    runBrowserRuntimeAction,
    updateBrowserSettings,
    type BrowserCapabilitiesSnapshot,
  } from '../web/agent-api';

  let snapshot = $state<BrowserCapabilitiesSnapshot | null>(null);
  let loading = $state(false);
  let loadError = $state('');
  let actionNotice = $state('');
  let savingSetting = $state<'inAppBrowserEnabled' | 'browserUseEnabled' | ''>('');
  let runtimeAction = $state<'check-updates' | 'install' | 'uninstall' | ''>('');
  let actionNoticeTimer: ReturnType<typeof setTimeout> | null = null;
  let runtimeStatusPollGeneration = 0;
  let disposed = false;

  function runtimeActionErrorMessage(error: unknown): string {
    if (error instanceof AgentApiError && error.message.trim()) {
      return error.message.trim();
    }
    if (error instanceof Error && error.message.trim()) {
      return error.message.trim();
    }
    return i18n.t('settings.browser.actionFailed');
  }

  const runtimeInstalled = $derived(snapshot?.runtimeStatus === 'installed'
    || snapshot?.runtimeStatus === 'update_available'
    || snapshot?.runtimeStatus === 'update_required');
  const runtimeInstallActionAvailable = $derived(snapshot?.runtimeStatus === 'not_installed'
    || snapshot?.runtimeStatus === 'failed'
    || snapshot?.runtimeStatus === 'update_available'
    || snapshot?.runtimeStatus === 'update_required');
  const runtimeInstallActionLabel = $derived(snapshot?.runtimeStatus === 'update_available'
    || snapshot?.runtimeStatus === 'update_required'
    ? i18n.t('settings.browser.update')
    : i18n.t('settings.browser.install'));
  const managementAvailable = $derived(Boolean(snapshot?.componentManagementAvailable));

  function clearActionNotice(): void {
    if (actionNoticeTimer !== null) {
      clearTimeout(actionNoticeTimer);
      actionNoticeTimer = null;
    }
    actionNotice = '';
  }

  function showActionNotice(message: string): void {
    clearActionNotice();
    actionNotice = message;
    actionNoticeTimer = setTimeout(() => {
      actionNotice = '';
      actionNoticeTimer = null;
    }, 2500);
  }

  function applySnapshot(next: BrowserCapabilitiesSnapshot): void {
    if (!snapshot || next.revision >= snapshot.revision) {
      snapshot = next;
    }
  }

  async function fetchSnapshot(): Promise<BrowserCapabilitiesSnapshot> {
    const next = await getBrowserCapabilities();
    applySnapshot(next);
    return next;
  }

  async function refresh(showNotice = false): Promise<void> {
    if (loading) return;
    loading = true;
    loadError = '';
    clearActionNotice();
    try {
      await fetchSnapshot();
      if (showNotice) {
        showActionNotice(i18n.t('settings.browser.refreshSucceeded'));
      }
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取浏览器运行组件状态失败:', error);
      loadError = i18n.t('settings.browser.loadFailed');
    } finally {
      loading = false;
    }
  }

  async function pollRuntimeStatus(generation: number): Promise<void> {
    while (!disposed && generation === runtimeStatusPollGeneration && runtimeAction) {
      try {
        await fetchSnapshot();
      } catch (error) {
        console.warn('[SettingsBrowserTab] 轮询浏览器运行组件状态失败:', error);
      }
      await new Promise<void>((resolve) => setTimeout(resolve, 350));
    }
  }

  function runtimeActionSuccessMessage(
    action: 'check-updates' | 'install' | 'uninstall',
    next: BrowserCapabilitiesSnapshot,
  ): string {
    if (action === 'install') {
      return i18n.t('settings.browser.installSucceeded');
    }
    if (action === 'uninstall') {
      return i18n.t('settings.browser.uninstallSucceeded');
    }
    if (next.availableRuntimeVersion) {
      return i18n.t('settings.browser.updateAvailable', {
        version: next.availableRuntimeVersion,
      });
    }
    return i18n.t('settings.browser.upToDate');
  }

  async function saveCapabilitySetting(
    key: 'inAppBrowserEnabled' | 'browserUseEnabled',
    enabled: boolean,
  ): Promise<void> {
    if (!snapshot || savingSetting) return;
    savingSetting = key;
    loadError = '';
    try {
      snapshot = await updateBrowserSettings({
        inAppBrowserEnabled: key === 'inAppBrowserEnabled'
          ? enabled
          : snapshot.inAppBrowserEnabled,
        browserUseEnabled: key === 'browserUseEnabled'
          ? enabled
          : snapshot.browserUseEnabled,
      });
      window.dispatchEvent(new CustomEvent('magi:browserCapabilitiesChanged', {
        detail: snapshot,
      }));
    } catch (error) {
      console.warn('[SettingsBrowserTab] 保存浏览器能力设置失败:', error);
      loadError = i18n.t('settings.browser.saveFailed');
    } finally {
      savingSetting = '';
    }
  }

  async function runRuntimeAction(action: 'check-updates' | 'install' | 'uninstall'): Promise<void> {
    if (runtimeAction || !managementAvailable) return;
    const generation = ++runtimeStatusPollGeneration;
    runtimeAction = action;
    loadError = '';
    clearActionNotice();
    const operation = runBrowserRuntimeAction(action);
    const statusPolling = pollRuntimeStatus(generation);
    try {
      const next = await operation;
      applySnapshot(next);
      window.dispatchEvent(new CustomEvent('magi:browserCapabilitiesChanged', {
        detail: next,
      }));
      showActionNotice(runtimeActionSuccessMessage(action, next));
    } catch (error) {
      console.warn(`[SettingsBrowserTab] 浏览器运行组件操作失败: ${action}`, error);
      const actionError = runtimeActionErrorMessage(error);
      await refresh();
      loadError = actionError;
    } finally {
      if (generation === runtimeStatusPollGeneration) {
        runtimeAction = '';
        runtimeStatusPollGeneration += 1;
      }
      await statusPolling;
    }
  }

  function statusText(): string {
    if (!snapshot) return i18n.t('settings.browser.status.unknown');
    return i18n.t(`settings.browser.status.${snapshot.runtimeStatus}`);
  }

  function hostStatusText(): string {
    if (!snapshot) return i18n.t('settings.browser.hostStatus.stopped');
    return i18n.t(`settings.browser.hostStatus.${snapshot.hostStatus}`);
  }

  function runtimeModeText(): string {
    if (!snapshot) return '-';
    return i18n.t(`settings.browser.mode.${snapshot.runtimeMode}`);
  }

  onMount(() => {
    disposed = false;
    void refresh();
    const handleRuntimeStatusChanged = (event: Event) => {
      const detail = (event as CustomEvent<{ eventType?: string }>).detail;
      if (detail?.eventType !== 'browser.runtime.status_changed') return;
      void refresh();
    };
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, handleRuntimeStatusChanged);
    return () => {
      disposed = true;
      runtimeStatusPollGeneration += 1;
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, handleRuntimeStatusChanged);
      clearActionNotice();
    };
  });
</script>

<div class="settings-tab-inner browser-settings-tab">
  <div class="browser-settings-content">
    <section class="capability-settings" aria-labelledby="browser-capabilities-title">
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
            checked={snapshot?.inAppBrowserEnabled ?? false}
            disabled={!snapshot || loading || Boolean(savingSetting)}
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
            checked={snapshot?.browserUseEnabled ?? false}
            disabled={!snapshot || loading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.browserUse')}
            onchange={(enabled) => void saveCapabilitySetting('browserUseEnabled', enabled)}
          />
        </div>
      </div>
    </section>

    <section class="runtime-summary" aria-labelledby="browser-runtime-title">
      <div class="runtime-heading">
        <div class="runtime-icon" aria-hidden="true"><Icon name="globe" size={18} /></div>
        <div>
          <h3 id="browser-runtime-title">{i18n.t('settings.browser.runtimeTitle')}</h3>
          <p>{i18n.t('settings.browser.runtimeDescription')}</p>
        </div>
        <span
          class:runtime-status--ready={snapshot?.hostStatus === 'ready'}
          class:runtime-status--busy={snapshot?.runtimeStatus === 'downloading'
            || snapshot?.runtimeStatus === 'verifying'
            || snapshot?.hostStatus === 'starting'}
          class="runtime-status"
        >
          <span aria-hidden="true"></span>{statusText()}
        </span>
      </div>

      <dl class="runtime-details">
        <div><dt>{i18n.t('settings.browser.runtimeMode')}</dt><dd>{runtimeModeText()}</dd></div>
        <div><dt>{i18n.t('settings.browser.hostStatusLabel')}</dt><dd>{hostStatusText()}</dd></div>
        <div><dt>{i18n.t('settings.browser.runtimeVersion')}</dt><dd>{snapshot?.runtimeVersion ?? '-'}</dd></div>
        <div><dt>{i18n.t('settings.browser.availableRuntimeVersion')}</dt><dd>{snapshot?.availableRuntimeVersion ?? '-'}</dd></div>
        <div><dt>{i18n.t('settings.browser.chromiumVersion')}</dt><dd>{snapshot?.chromiumVersion ?? '-'}</dd></div>
        <div><dt>{i18n.t('settings.browser.playwrightVersion')}</dt><dd>{snapshot?.playwrightVersion ?? '-'}</dd></div>
        <div><dt>{i18n.t('settings.browser.hostVersion')}</dt><dd>{snapshot?.hostVersion ?? '-'}</dd></div>
      </dl>

      {#if loadError}
        <div class="runtime-error" role="status">
          <Icon name="alert-circle" size={14} />
          <span>{loadError}</span>
        </div>
      {:else if snapshot?.lastErrorCode}
        <div class="runtime-error" role="status">
          <Icon name="alert-circle" size={14} />
          <code>{snapshot.lastErrorCode}</code>
        </div>
      {:else if actionNotice}
        <div class="runtime-feedback" role="status" aria-live="polite">
          <Icon name="check-circle" size={14} />
          <span>{actionNotice}</span>
        </div>
      {/if}
    </section>

    <section class="runtime-actions" aria-labelledby="browser-runtime-actions-title">
      <div class="section-heading">
        <div>
          <h3 id="browser-runtime-actions-title">{i18n.t('settings.browser.managementTitle')}</h3>
          <p>{snapshot?.runtimeMode === 'development'
            ? i18n.t('settings.browser.developmentManagedExternally')
            : i18n.t('settings.browser.managementDescription')}</p>
        </div>
        <button
          type="button"
          class="icon-action"
          class:icon-action--loading={loading}
          onclick={() => void refresh(true)}
          disabled={loading}
          title={i18n.t('settings.browser.refreshStatus')}
          aria-label={i18n.t('settings.browser.refreshStatus')}
        ><Icon name="refresh" size={14} /></button>
      </div>

      <div class="action-list">
        <div class="action-row">
          <div><strong>{i18n.t('settings.browser.install')}</strong><span>{i18n.t('settings.browser.installDescription')}</span></div>
          <button
            type="button"
            onclick={() => void runRuntimeAction('install')}
            disabled={!managementAvailable || !runtimeInstallActionAvailable || Boolean(runtimeAction)}
          >{runtimeAction === 'install' ? i18n.t('settings.browser.installing') : runtimeInstallActionLabel}</button>
        </div>
        <div class="action-row">
          <div><strong>{i18n.t('settings.browser.checkUpdates')}</strong><span>{i18n.t('settings.browser.checkUpdatesDescription')}</span></div>
          <button
            type="button"
            onclick={() => void runRuntimeAction('check-updates')}
            disabled={!managementAvailable || Boolean(runtimeAction)}
          >{runtimeAction === 'check-updates' ? i18n.t('settings.browser.checkingUpdates') : i18n.t('settings.browser.checkUpdates')}</button>
        </div>
        <div class="action-row">
          <div><strong>{i18n.t('settings.browser.uninstall')}</strong><span>{i18n.t('settings.browser.uninstallDescription')}</span></div>
          <button
            class="danger"
            type="button"
            onclick={() => void runRuntimeAction('uninstall')}
            disabled={!managementAvailable || !runtimeInstalled || Boolean(runtimeAction)}
          >{runtimeAction === 'uninstall' ? i18n.t('settings.browser.uninstalling') : i18n.t('settings.browser.uninstall')}</button>
        </div>
      </div>

      {#if !managementAvailable}
        <p class="management-note">{i18n.t('settings.browser.managementUnavailable')}</p>
      {/if}
    </section>
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

  .runtime-summary,
  .runtime-actions,
  .capability-settings {
    padding: 0 2px 26px;
  }

  .runtime-summary,
  .runtime-actions {
    padding-top: 26px;
    border-top: 1px solid var(--ind-border-separator);
  }

  .runtime-heading,
  .section-heading,
  .action-row,
  .capability-row {
    display: flex;
    align-items: center;
  }

  .runtime-heading,
  .section-heading {
    gap: 12px;
  }

  .runtime-heading > div:nth-child(2),
  .section-heading > div,
  .capability-row > div {
    min-width: 0;
    flex: 1;
  }

  .capability-list,
  .action-list {
    border-top: 1px solid var(--ind-border-separator);
  }

  .capability-row {
    min-height: 58px;
    gap: 20px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .capability-row strong,
  .capability-row span {
    display: block;
  }

  .capability-row strong {
    color: var(--ind-foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .capability-row span {
    margin-top: 4px;
    color: var(--ind-foreground-secondary);
    font-size: 11px;
    line-height: 1.45;
  }

  .runtime-icon {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border: 1px solid var(--ind-border-control);
    border-radius: 8px;
    color: var(--ind-tab-accent);
    background: var(--ind-bg-control);
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

  .runtime-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--ind-foreground-muted);
    font-size: 12px;
    white-space: nowrap;
  }

  .runtime-status > span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ind-foreground-soft);
  }

  .runtime-status--ready > span { background: var(--success, #2f9e63); }
  .runtime-status--busy > span {
    background: var(--ind-tab-accent);
    animation: browser-runtime-status-pulse 1.1s ease-in-out infinite;
  }

  @keyframes browser-runtime-status-pulse {
    50% { opacity: 0.35; }
  }

  .runtime-details {
    margin: 24px 0 0;
    border-top: 1px solid var(--ind-border-separator);
  }

  .runtime-details > div {
    min-height: 39px;
    display: grid;
    grid-template-columns: minmax(150px, 1fr) minmax(0, 1.5fr);
    align-items: center;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  dt, dd { margin: 0; font-size: 12px; }
  dt { color: var(--ind-foreground-secondary); }
  dd {
    overflow-wrap: anywhere;
    color: var(--ind-foreground);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    text-align: right;
  }

  .runtime-error,
  .runtime-feedback {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    font-size: 12px;
  }

  .runtime-error { color: var(--danger, #c53f4f); }
  .runtime-feedback { color: var(--success, #2f9e63); }

  .icon-action--loading :global(svg) {
    animation: browser-runtime-refresh-spin 0.9s linear infinite;
  }

  @keyframes browser-runtime-refresh-spin {
    to { transform: rotate(360deg); }
  }

  .section-heading { margin-bottom: 12px; }

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

  .action-row {
    min-height: 62px;
    gap: 18px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .action-row > div { min-width: 0; flex: 1; }
  .action-row strong { display: block; color: var(--ind-foreground); font-size: 12.5px; }
  .action-row span { display: block; margin-top: 4px; color: var(--ind-foreground-muted); font-size: 11.5px; line-height: 1.45; }
  .action-row button { min-width: 76px; min-height: 29px; padding: 0 10px; font-size: 12px; }
  .action-row button.danger:not(:disabled) { color: var(--danger, #c53f4f); }
  button:disabled { cursor: default; opacity: 0.42; }
  .management-note { margin-top: 12px; }

  @media (max-width: 560px) {
    .browser-settings-tab { padding-inline: 18px; }
    .runtime-heading { align-items: flex-start; flex-wrap: wrap; }
    .runtime-status { margin-left: 46px; }
    .runtime-details > div { grid-template-columns: minmax(105px, 1fr) minmax(0, 1.2fr); }
  }
</style>
